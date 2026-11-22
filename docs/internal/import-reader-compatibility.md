# Inactive import reader compatibility

This is evidence for [issue 146](https://github.com/yuxhuang/r-orch/issues/146),
not a change to the import contract. The reader has no runtime caller.
The governing documents remain the [spec](../specs/2026-11-20-sqlite-storage.md)
R109–R112, R122–R123 and AP15, the [plan](../plans/2026-11-20-sqlite-storage.md)
T21, and [ADR 0017](../decisions/0017-sqlite-storage.md) decision 21.

## Config decision before wiring

`effective_config_defaults_and_r109_document_count_are_distinct` writes only disposable
synthetic files and calls `FileConfigStore::read_merged`, the effective 1.0 config
reader. It compares its result with the actual default configuration, not a copied
list of default values. The bounded side uses the same bytes.

| Source config | Effective legacy reader | Current bounded document reader |
|---|---|---|
| Missing | Defaults | No stream to check; missing-file handling belongs to T21 |
| Empty | Defaults | Refuses zero documents |
| Spaces and newlines only | Defaults | Refuses zero documents |
| Comment only | Defaults | Refuses zero documents |
| Explicit `null` | Defaults | One document; value is null |
| `{}` | Defaults | One document; value is an empty mapping |
| Two mappings separated by `---` | Refuses | Refuses multiple documents |

Null passing the budget pass does not make it a valid task: typed `Task` decoding
refuses it. A tab at the start of a YAML line is not equivalent to spaces; the legacy
reader rejected that initial test sample as scanner syntax. The whitespace row uses
spaces and newlines.

**Decision required of the spec owner:** retain R109 strictly and disclose that a
present zero-document config which 1.0 reads as defaults cannot be imported, or approve
a narrowly scoped config-default exception and amend R109, the R110 table and AP15's
compatibility statement together. A missing config and explicit null need separately
specified mapping/default handling; neither is a zero-document stream. Recommendation:
consider the narrow exception for config only, keeping every byte and parse bound.
No exception is implemented here. Until disposition, the zero-document refusal says
`expected one document, found 0`; the future reporting layer must supply the escaped
source-relative file name. The existing R110 table's “The same / None” document-count
row does not describe the effective config reader for empty streams.

## Compatibility conditions

| Item from issue 146 | Evidence or remaining condition |
|---|---|
| I1 | Config matrix above pins the discrepancy. Spec-owner disposition and named-path dry-run reporting remain prerequisites for wiring. |
| N1 | `physical_blank_lines_form_gaps_while_legacy_skips_unicode_whitespace` compares real `FileRunStore` reads. Empty, ASCII, U+2003 and U+00A0 lines with CRLF join adjacent malformed lines into physical gap 2–4; valid events retain lines 1 and 5. The final newline creates no extra line. The legacy reader returns two events. R111 is unchanged. |
| N2 | `index_damage_distinguishes_unreadable_streams_from_bad_array_entries` checks missing, malformed JSON, object and null indexes produce unreadable + file ID order. Non-object array entries, absent/non-string identities and duplicates refuse with `index_entry`, without writes by the bounded path. Real typed `IndexManager::read_index` rebuilds the first seven cases on disposable copies; decoded duplicates survive. Existing corpus tests retain stale-file content and distinct legacy/import order checks. Explicit R122/R123 problem-table disposition for bad array entries remains open. |
| N3 | Named `JSON_DEPTH_CLAMP = 64`, pinned at 64/65 even when the requested limit is 100. No limit was raised. |
| N4 | `interrupted_buffered_reads_retry_and_terminal_errors_end_the_stream` injects one Interrupted after a partial buffered line and verifies both events survive. A PermissionDenied at the same position yields an I/O refusal and terminates, rather than a gap. Document `read_to_end` retries Interrupted and propagates terminal failure too. |
| N5 | Existing `depth_bombs_are_refused_on_a_small_stack` pins the YAML parser's depth classification. Its scanner's “recursion limit exceeded” wording is still a dependency; parser updates must preserve that test or deliberately review the classification. |
| N6 | No async runtime caller added. `parse_yaml` still performs synchronous bounded input/budget work before the guarded legacy parser's blocking task. Future integration must acquire and budget documents outside async executor threads; this slice does not prove scheduling or cancellation at that boundary. |
| N7 | Existing generated corpus test parses 50 documents and 76 event lines and verifies 73 files unchanged. Existing typed Task, Agent, state and RunEvent examples remain. Whole-source R109–R116 tests, shared alias/merge integration accounting, production-limit child outcomes/RSS, confinement, writer, dry run and resume remain T21 acceptance work. No live project was read. |

Rollback is reverting this inactive slice. There is no database migration, backfill,
source transformation or format cutover. No new CLI, database, source walk or runtime
caller is introduced. These assertions are reader-level evidence, not completion of
any release gate.
