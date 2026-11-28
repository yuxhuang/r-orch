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

`teams/` and `runs/` have no index. The first offline `orch import` caller runs this
preflight before its native template writer. It writes templates only, supports resume
of those templates, and reports every writing run as unfinished. The spec's
[offline caller contract](../specs/2026-11-20-sqlite-storage.md#offline-importer-first-caller-as-built)
lists the unsupported families and remaining obligations. The preflight itself creates,
changes and removes nothing, and opens no path built from a value inside a document or
an index.

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
| A tab at the start of a line (C-V5) | Rejects it as scanner syntax | Refuses as `unreadable`; says nothing about 1.0. A file that does not scan is never counted as a stream of no document. The bounded reader reaches this from `parse` (limit 0, observed 0), not from `documents` (PR 239) |
| Two mappings separated by `---` | Refuses | Refuses under `documents`; says nothing about 1.0 |
| A mapping | The mapping over the defaults | The mapping over the frozen 1.0 defaults, below |
| A mapping with a numeric or boolean key, at the top or inside another mapping (`200: x`, `true: y`) | Reads the key as the string `"200"` or `"true"` | The same strings. `extras: {200: x, true: y}` with a top-level `200: top` gives the unknown keys `200`, `extras.200` and `extras.true`, and no refusal |
| A mapping with a null key (`extras: {~: x}`) | Refuses | Refuses as `unreadable` |
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
value the source set. It is a note and stops nothing. `run_cap` is `null` in the report's
JSON if the config check did not finish; the member is not omitted.

**Unknown keys.** A key that is not in the frozen defaults is unknown, at any depth: an unknown key inside
a known section is unknown too, and `prompt` has the known children `template`,
`system_template` and `user_template`. Each unknown path is listed as a note
`unknown_config` with a dotted path. Each component is first limited to 256 characters,
the joined path is then cut to 256 characters overall, and only then is it escaped, so a
long path does not keep 256 characters of every component.
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
trust. The bounded template caller reports this preview but does not import settings or enable the policy. Owner confirmation before enabling it remains final T21 work (R93/R133).

## Indexes

Each directory with an index is reconciled one index and one entity document at a time.
An entity file's content is taken from its file, never from its index.

| Index | Result |
|---|---|
| Missing; not JSON within its bounds; a JSON object; `null` | Note `index_unreadable`; entity files are ordered by id; the check goes on |
| `[]` | A usable index that lists nothing: not `index_unreadable`; each file is noted `index_unlisted` and ordered by id |
| An array with any entry that is not an object or has no string identity (`id`, and `key` under `context/`) | Refuses `index_entry`, saying that 1.0 rebuilt such an index |
| An array of string identities with one identity more than once, whether the entries are bare identities or whole entities, and whether or not a file exists | Refuses `index_duplicate`, naming the identity (escaped, cut), saying that 1.0 used such an index as it stood only if every entry was a complete entity of the directory's type and otherwise rebuilt it |
| An entry whose file is missing | Note `index_missing_file`; its identity is never opened as a path. A file that is present but refused (for example a `null` document) gets its own refusal and is not also reported as missing (`an_unreadable_entity_is_not_reported_as_a_missing_file`) |
| A file the index does not list | Note `index_unlisted`; ordered after the listed entities, in id order |
| An entry that differs from its file | Note `index_differs` |
| Over 16,777,216 bytes, nested more than 64 deep, over 1,000,000 nodes; a symbolic link, a FIFO or a file with two hard links; an input or output error | Terminal stop (`bytes`, `depth`, `nodes`, `file_kind`, `io`); never `index_unreadable` |

The array is examined whole before a class is chosen (R122): a bad entry anywhere decides
`index_entry`, and an earlier repeated identity cannot hide it. Two identities that differ
only in case are two (`A` and `a`). The preflight judges an entry by its identity alone and
decodes no entry as an entity type.

**Id order.** Files with no index entry, and every file when the index is missing, `[]` or unreadable, are
ordered by their id: the file name with its extension removed (`.yml` for tasks, agents and goals, `.json` for
messages and context), compared as bytes. The extension must not take part: as file names `a-b.json` sorts before
`a.json`, but as ids `a` precedes `a-b`. In every one of the five directories the files `a`, `a-b`, `a.b` and
`a_b` are listed in that order with no index and with `[]`. With an index that lists only `a_b`, `a_b` is first
and the other three follow in that id order.

**Opaque fields of an index entry.** The preflight takes an index entry's identity, decodes no entry as an entity type and
reads the rest of the entry as raw JSON, so a field of a valid entry that holds a number outside the range of a JSON value
(`1e999`) is neither a refusal nor a reason to change the order. Three rules follow:

- *Acceptance stays native.* A file that the typed reader accepts is accepted even if an ignored field of it
  holds `1e999`; the same number in a field the type knows (`value` of a context entry) is refused, as before.
- *Whole-array precedence is unchanged.* In every directory, `[{id:b, x:1e999}, {id:b}, 0]` and
  `[0, {id:b, x:1e999}, {id:b}]` each give one refusal, `index_entry`, never `index_duplicate`; the position of
  the malformed entry does not matter (`key` replaces `id` under `context/`). An array of `{id:b, x:1e999}` and
  `{id:a}` is usable and orders `b`, `a`, then the unlisted file `c`.
- *Comparison with the file (`index_differs`) is structural.* Objects compare by member, ignoring order, spacing
  and string escapes (`"\u0061"` equals `"a"`); arrays compare element by element in order; a number that
  does not fit a JSON value compares by its text (`1e999` is not `2e999`). A different number, or the same
  elements in another order, notes `index_differs`; the same content in another layout does not. The order and the
  count of the entities are unchanged either way.

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
| C-V2 to C-V4, C-V6, C-V9, C-V12 and the C-V13 wording, and the preflight's result for the tab-only stream of C-V5 | `config_empty_null_and_non_mapping_diagnostics_match_the_contract` |
| C-V1, C-V7, C-V8, C-V14 | `omitted_config_run_cap_uses_frozen_legacy_default_and_a_note` |
| C-V10, X-V12, X-V14 | `budgets_and_file_kind_stops_never_become_index_unreadable` |
| C-V11 | `an_entity_file_with_no_document_is_refused_as_1_0_refuses_it`, which also runs 1.0's `FileTaskStore`, `FileAgentStore`, `FileGoalStore` and `FileTeamStore` `get` on a separately created zero-byte file for each and requires each to error, as the preflight does |
| C-V5 | `a_config_that_does_not_scan_stops_before_its_documents_are_counted`, a unit test of the bounded reader in `crates/orch-infra/src/import/tests/compatibility.rs` (PR 239). It runs six tab-only streams through 1.0's `FileConfigStore::read_merged` (each fails) and requires rule `parse`, limit 0 and observed 0 from `check_yaml`, `parse_yaml` and `decode_yaml`, while five streams that scan and hold no document keep `documents`. The preflight's result for one such stream, `unreadable` with no statement about 1.0, is a row of the C-V2 test above |
| X-V4, X-V6 | `whole_array_bad_entry_precedes_duplicates_in_all_five_directories` |
| X-V5, X-V7 | `both_duplicate_shapes_and_missing_files_stop_in_all_five_directories`, which also runs 1.0's `IndexManager::read_index` on a second, disposable source for each case in each of the five directories: it keeps a complete-entity duplicate and rebuilds an array of identities |
| X-V1 to X-V3, X-V8 to X-V11 | `index_order_and_named_problems_use_files_and_never_open_index_identities` |
| Unknown keys, the 64 KiB bound | `unknown_config_is_inert_bounded_and_values_are_not_reported` |
| Preapproval | `auto_agents_require_owner_confirmation_without_granting_policy` |
| Escaping and cutting | `hostile_duplicate_identity_is_escaped_and_cut_without_changing_class` |
| Entity documents decode as before | `typed_json_documents_still_refuse_duplicate_identity_members`, `typed_yaml_entities_keep_ignored_yaml_only_fields` |
| Report caps | `order_and_notes_are_capped_without_changing_counts` |
| A present file that is refused is not also a missing file | `an_unreadable_entity_is_not_reported_as_a_missing_file` |
| Numeric and boolean YAML keys read as 1.0 reads them (issue 270) | `numeric_and_boolean_yaml_keys_follow_the_effective_legacy_reader`, which runs 1.0's `FileConfigStore::read_merged` on the same bytes and requires the strings `200` and `true`, then the unknown keys `200`, `extras.200` and `extras.true` from the preflight, then a null key refused by both |
| Id order does not depend on the document extension (issue 270) | `prefix_id_order_is_independent_of_the_document_extension_in_every_store`: five directories, each with no index, `[]` and an index that lists `a_b` |
| Ignored opaque JSON numbers (issue 270) | `opaque_ignored_json_numbers_preserve_acceptance_order_and_structural_comparison`: a context file with an ignored `1e999` is accepted by the typed reader and by the preflight, three index variants give `index_differs` 0, 1 and 1 times with the order unchanged, and the same number in the known `value` field is refused |
| Opaque index members and whole-array precedence (issue 270) | `opaque_index_members_do_not_change_whole_array_precedence_in_any_store`: both entry positions of the malformed entry in all five directories, then the usable array |

X-V13 is a unit test of the module, where a reader can be injected:
`an_index_read_error_stops_and_an_interrupted_read_is_retried`, in
`crates/orch-infra/src/import/preflight/compatibility.rs`. It calls `Check::index_read`,
the method `index_plan` reads every index through, with a reader that fails in the middle
of the stream. A `PermissionDenied` stops with an `io` refusal on the index's path and is
never noted as `index_unreadable`; one `Interrupted` is retried and gives the result of
an undisturbed read.

C-V6's bare `---` is pinned by the test of C-V2 to C-V6: the preflight's pass counts it
as one null document. The tests also pin two points on the entity documents: a JSON
message or context document that repeats a field the entity type knows is still refused as `unreadable`, and
a YAML entity keeps an extra YAML-only key that the typed reader ignores.

## Compatibility conditions

These reader-level checks pre-date the wiring and still stand.

| Item from issue 146 | Evidence or remaining condition |
|---|---|
| I1 | The config table above pins the discrepancy; the contract on `main` disposes of it. The named-path report is the preflight's. Integrated dry-run output, including the `.orchestry/` prefix on a printed path, is the command's. |
| N1 | `physical_blank_lines_form_gaps_while_legacy_skips_unicode_whitespace` compares real `FileRunStore` reads. Empty, ASCII, U+2003 and U+00A0 lines with CRLF join adjacent malformed lines into physical gap 2–4; valid events retain lines 1 and 5. The final newline creates no extra line. The legacy reader returns two events. R111 is unchanged. |
| N2 | `index_damage_distinguishes_unreadable_streams_from_bad_array_entries` runs the bounded reader on missing, malformed JSON, object and null indexes, and on non-object entries, absent or non-string identities and duplicates, and real `IndexManager::read_index` rebuilds the first seven cases on disposable copies. That test decodes entries into a type of one field, so its "decoded duplicates survive" result does not hold for a real directory: an array of identities alone does not decode as the entity type, and 1.0 rebuilds it (spec F19). That reader-level `reconcile` stops at the first problem it meets and names a repeated identity `index_entry`, so its class depends on the order of the entries; the preflight does not call it and follows R122 and R123 instead (`index_entry` first, then `index_duplicate`), as the preflight tests above show. Existing corpus tests retain stale-file content and distinct legacy/import order checks. |
| N3 | Named `JSON_DEPTH_CLAMP = 64`, pinned at 64/65 even when the requested limit is 100. No limit was raised. |
| N4 | `interrupted_buffered_reads_retry_and_terminal_errors_end_the_stream` injects one Interrupted after a partial buffered line and verifies both events survive. A PermissionDenied at the same position yields an I/O refusal and terminates, rather than a gap. Document `read_to_end` retries Interrupted and propagates terminal failure too. |
| N5 | Existing `depth_bombs_are_refused_on_a_small_stack` pins the YAML parser's depth classification. Its scanner's "recursion limit exceeded" wording is still a dependency; parser updates must preserve that test or deliberately review the classification. |
| N6 | The preflight acquires and budgets every document on one blocking thread, never on an async executor thread, and a flag stops it between reads. It does not prove cancellation of a blocked kernel read, or that a caller holds the 1.0 lock for as long as it relies on the answer; both wait for the first importer caller (issue 192). |
| N7 | Existing generated corpus test parses 50 documents and 76 event lines and verifies 73 files unchanged. Existing typed Task, Agent, state and RunEvent examples remain. Production-limit child outcomes and RSS are in `source_budget`. Integrated dry run, resume, writer and no-write acceptance remain T21 acceptance work (issue 146). No live project was read. |

## Command-level synthetic checks

`crates/orch-cli/tests/import_reader_compatibility.rs` runs the actual offline command,
with a synthetic source, private process environment and native template destination.
For a source refusal it compares the complete preflight report between dry-run and
writing modes, checks exit 2, zero imported batches and `unfinished: false`, and observes
source bytes, inventory, inodes, modes, link counts and write times. It tests both an absent
destination and a native destination seeded by a real template import; a refused resume
must preserve the destination's bytes, inventory and all observed metadata, including
access time; the observer uses `O_NOATIME` for file reads and directory descriptor
enumeration. Source access time is excluded because reading may advance it. No timestamp
is restored to hide a change.

| Vectors | Command test |
|---|---|
| C-V2–6, C-V9, C-V12–13; bare document-end marker | `c_v2_to_v6_v9_v12_v13_refusals_match_in_both_modes_without_writes` |
| C-V1, C-V7–8, C-V14 | `c_v1_v7_v8_v14_defaults_and_explicit_caps_match_before_template_writes` |
| C-V11 | `c_v11_empty_typed_documents_keep_zero_document_wording_without_writes` |
| X-V4–7, including identity-only and complete duplicates in all five stores | `x_v4_to_v7_bad_entries_precede_both_duplicate_shapes_without_writes` |
| X-V1–3, X-V8–11 | `x_v1_to_v3_v8_to_v11_index_order_is_previewed_identically_before_template_writes` |
| C-V10; X-V12 bytes, depth and nodes | `c_v10_and_x_v12_byte_limits_refuse_before_creating_or_changing_destination`, `x_v12_depth_and_node_boundaries_keep_budget_precedence_in_both_modes` |
| X-V14 | `x_v14_symlinks_fifos_and_hard_links_are_terminal_prewrite_refusals` |
| Terminal confined open error | `terminal_index_open_error_is_io_and_never_an_unreadable_index` |
| Bounded escaped duplicate identity | `hostile_duplicate_diagnostics_are_bounded_and_escaped_in_both_modes` |
| Tab-led content, vertical tab, ESC, BOM then tab | `tab_led_content_vertical_tab_escape_and_bom_tab_refuse_without_legacy_defaults` |
| R111 adjacent physical gaps | `r111_adjacent_blank_and_malformed_lines_form_one_gap_in_both_mode_reports` |
| Preapproval preview only | `dry_run_auto_policy_is_a_confirmation_preview_and_creates_nothing` |

The X-V5 legacy comparisons remain in
`import_index_reference::typed_duplicates` and the preflight test
`both_duplicate_shapes_and_missing_files_stop_in_all_five_directories`: a complete typed
duplicate is served twice and an identity-only array is rebuilt. These command tests
reuse those two shapes to prove native caller refusal, without rerunning legacy startup
against the import source. The original one-field fixture still has only the scope stated
under N2. X-V13 interrupted and mid-stream terminal reads remain injected unit tests;
the command-level open-error test does not replace that proof.

Successful checks compare the same preflight preview before writing a template; they
prove neither persistence of the previewed index order nor import of config settings or
other entities. R111 checks report two events and one adjacent physical gap; storing their
line numbers remains the full event importer's obligation. The existing
`import_offline` command suite separately checks native template rollback, retained
manifest/hash validation and resume. None of this is live-copy evidence.

## Still open

Issue 146 remains open for final T21 acceptance. Its bounded template caller tests
supply config/index preflight parity and refusal evidence, not a complete import.

- Full-family import, config and index-order persistence, R93/R133 owner confirmation
  before enabling preapproval, state/events/attachments, large-body registration,
  R121/R125 and completion stay with T21. A successful preview imports no policy.
- The full importer still needs its integrated production-budget child-process time
  and peak-RSS evidence, terminal-read rollback, checkpoint/resume equivalence and
  source immutability. `source_budget` measures the preflight, not the full importer.
- First-caller blocked-read cancellation and lock-lifetime acceptance stay with issue
  192. The bounded caller's native-writer cancellation tests cover their stated surface.
- The reader-level `import::index::reconcile` remains an older, unwired classifier. It
  disagrees with the contract on a repeated identity and gathers every entity document
  of a directory; the actual preflight does neither. Until removed, two classifiers exist.
- A copy of the owner's live project requires separate permission. No test here reads it.

This test change adds no format cutover or runtime behavior. The template caller's
unfinished destination is not a usable imported project, and it writes no project pointer.
