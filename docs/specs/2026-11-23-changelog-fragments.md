# Changelog fragments and release assembly

status: draft
owner: tech-writer (the documentation writer of this wave); the owner approves by merging
date: 2026-11-23
plan: [../plans/2026-11-23-changelog-fragments.md](../plans/2026-11-23-changelog-fragments.md)
decision: [ADR 0021](../decisions/0021-changelog-fragments.md)

## Source

> 1. Base independent PRs on main and qualify them together in trains. Stack only when the upper change requires
>    the lower PR's code. Document the dependency and admit the entire stack to one train, ordered bottom to top
>    (e.g. #171 then #175, #186 then #208). If any member is held, defer the stack together. A changed head
>    requires delta review, fresh CI and a renewed recommendation. Keep shared specs and plans with a single
>    documentation writer per wave. Builders own disjoint implementation paths.
> 2. Define the contributor fragments and the release assembly: a precise assembler command under `xtask/`,
>    the version input, the fragment format and PR-number bootstrap, the treatment of existing Unreleased notes,
>    consumed fragments and compare links, with an example that needs no second PR.

Owner decision of 2026-11-23, approved at the mid tier with a balanced stance (item 1 is quoted; item 2 condenses
the brief for the documentation slice). Existing unreleased notes must survive the cutover.

## Why

Every PR that edits `## [Unreleased]` in `CHANGELOG.md` edits the same lines (open PRs 207 and 214 both do, on
2026-11-23). Two such PRs can conflict even when their code does not, and a train must then defer one of them.
One file per entry removes the shared lines.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check |
|---|---|---|---|---|
| R1 | A change a user could notice shall add one or more fragment files under `docs/changelog/unreleased/` and shall not edit `CHANGELOG.md`. | 2 | must | AGENTS.md "Changelog fragments"; review. |
| R2 | A fragment name shall match `<slug>.<kind>.md`: `<slug>` is lowercase ASCII letters and digits in groups joined by single `-`, at most 64 bytes; `<kind>` is one of `added`, `changed`, `deprecated`, `removed`, `fixed`, `security`. | 2 | must | `xtask/tests/changelog.rs`, one test per refusal; Linux CI. |
| R3 | A fragment shall be a regular file (not a symlink) of valid UTF-8 holding exactly one line of 1 to 1,000 bytes ended by one LF; no other control character, no U+2028, U+2029 or bidirectional control (U+202A to U+202E, U+2066 to U+2069); no leading or trailing space; the first character an ASCII letter, or a backtick not followed by a backtick. | 2 | must | same; one test per rule. |
| R4 | The fragment directory shall hold nothing but fragments and one `README.md`, at most 500 fragments; any other entry (subdirectory, symlink, stray name) shall be refused by name. A missing directory means zero fragments. | 2 | must | same. |
| R5 | `cargo xtask changelog check` shall validate R2 to R4 on the working tree, change nothing, exit 0 when valid and nonzero naming each offending file otherwise. | 2 | must | same. |
| R6 | `cargo xtask changelog assemble --version X.Y.Z --date YYYY-MM-DD [--dry-run]` shall require both options; `X.Y.Z` is three decimal numbers without leading zeros and without a `v` or a suffix, strictly greater than the newest released version in `CHANGELOG.md`, absent from it, and equal to `workspace.package.version` in `Cargo.toml`; the date is a real calendar date and the tool never reads the clock. | 2 | must | same. |
| R7 | `assemble` shall turn the `## [Unreleased]` heading into a new `## [X.Y.Z] - YYYY-MM-DD` section holding, in this order: the existing Unreleased text unchanged, then one `### <Kind>` subsection per non-empty kind in the order Added, Changed, Deprecated, Removed, Fixed, Security, each entry `- <fragment line>`, sorted by file name by byte value; and add a new empty `## [Unreleased]` above it. | 2 | must | golden test with the example below; Linux CI. |
| R8 | `assemble` shall carry the existing Unreleased text byte for byte (leading and trailing blank lines excepted): no reordering, deduplication, re-wrapping or heading change. | 2 | must | golden test with a legacy body. |
| R9 | `assemble` shall point `[Unreleased]:` at `compare/vX.Y.Z...HEAD` and add `[X.Y.Z]: <base>/compare/v<previous>...vX.Y.Z` directly under it, where `<base>` and `<previous>` come from the existing `[Unreleased]:` line and the newest released heading must agree with it; otherwise refuse. | 2 | must | golden test; refusal test. |
| R10 | `assemble` shall write `CHANGELOG.md` through a temporary file in the same directory and an atomic rename, and only then delete the fragments it consumed. A refusal before the rename shall leave every file unchanged. | 2 | must | failure-injection test; Linux CI. |
| R11 | `assemble` with no fragment and no existing Unreleased text shall refuse. With only legacy text it shall proceed. | 2 | must | tests. |
| R12 | `assemble --dry-run` shall print the new section to standard output, write nothing and delete nothing. | 2 | must | test. |
| R13 | A fragment-only change shall classify as `docs-only` in `cargo xtask gate select`. | 2 | must | `cargo xtask gate select --base <base> --format json` on a fragment-only diff shows `"mode": "docs-only"`; Linux. |
| R14 | CI's `check` job should run `cargo xtask changelog check`, so a malformed fragment is refused before it reaches `main` rather than at release. | 2 | should | workflow diff; owner merges (see Open questions). |
| R15 | The independent-PR, stack and train rules of Source item 1 shall be in AGENTS.md "Independent PRs, stacks and trains". | 1 | must | review. |
| R16 | The historical SQLite spec, plan and ADR 0017 shall stay as written. | 2 | must | `git diff` of the three files is empty. |

## Format

Path: `docs/changelog/unreleased/<slug>.<kind>.md`. Content: the entry, one line, user-voiced, as in AGENTS.md.

```
docs/changelog/unreleased/socket-timeout.fixed.md
Fix the client socket timeout so a slow daemon reply no longer ends a command early.
```

- Name the slug after the branch, so the first push already contains the fragment. Two files with the same slug
  and different kinds are allowed; two entries of one kind need two slugs (`socket-timeout.fixed.md`,
  `socket-retry.fixed.md`). A repeated name is an ordinary add/add conflict in the PR, which is the signal to
  pick another slug.
- No PR number is needed. The fragment is created before the PR exists, and pushing a second commit only to add
  the number would change the head (a changed head restarts review, CI and the recommendation). The number is not
  rendered. While the file exists, `git log --follow -- <fragment>` lists the commit that added it, and a squash
  commit's title ends with `(#N)`. An author who wants an issue link writes it inside the line.
- Why the first-character rule: the line is rendered as `- <line>`, and Markdown would turn a leading `#`, `>`,
  `-`, `*`, `+`, a digit followed by `.`, or a fence into a heading, quote, list or code block inside the bullet.

## Release assembly

Command (run after the version bump of Release step 2, from the repository root, on the release branch):

```
cargo xtask changelog assemble --version X.Y.Z --date YYYY-MM-DD [--dry-run]
```

1. Validate the fragment directory as `check` does. Any refusal stops here.
2. Read `CHANGELOG.md` (a regular file, at most 8 MiB, UTF-8). Require exactly one line `## [Unreleased]`, exactly
   one `[Unreleased]: <base>/compare/v<previous>...HEAD` line, and that `<previous>` is the version of the first
   `## [A.B.C] - ` heading below it. The existing Unreleased text is everything from the line after
   `## [Unreleased]` to the line before the next `## [` heading.
3. Build the new section and links (R7 to R9) in memory.
4. Write `CHANGELOG.md.tmp` beside it, flush it, rename it over `CHANGELOG.md` (R10).
5. Delete each consumed fragment. If a deletion fails, report which and exit nonzero: the changelog is already
   correct, and the leftover fragment must be deleted by hand or it would be assembled into the next release.
   `README.md` is never deleted.
6. Print one line per step, for example `changelog: assembled 1.0.1 (2026-12-01): 1 entry (Fixed 1); existing
   Unreleased text: none`, `changelog: wrote CHANGELOG.md`, `changelog: removed <path>`.

The commit that the release engineer makes then holds `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and the deleted
fragments together (Release step 5).

### Existing Unreleased notes at the cutover

On the cutover, `## [Unreleased]` in `CHANGELOG.md` holds hand-written notes (some under non-standard headings,
and a closing paragraph about PR2 that was written for the gate work). They are not converted to fragments: the
conversion would drop their structure and the history around them. They stay in place and `assemble` carries them
into the first release's section as R8 says. Consequences, stated so nobody is surprised:

- That section can show a legacy `### Added` followed by a generated `### Added`. Only a human edit in the
  release PR merges them; the assembler never does. Pull requests opened before the cutover that still add a line
  under `[Unreleased]` stay valid; they may switch to a fragment if they conflict.
- After that release, `[Unreleased]` is empty and stays empty until the next one. No PR edits it.

### Example with no second PR

One PR, one fragment, one release. Illustrative: the fragment text is invented, and the output below is the
specified output that the golden test in `xtask/tests/changelog.rs` must reproduce byte for byte. It has not yet
been produced by the tool. (When this spec is marked `done`, replace it with a recorded run.)

`CHANGELOG.md` before (excerpt; `Cargo.toml` already says `1.0.1`):

```
## [Unreleased]

## [1.0.0] - 2026-11-14
...
[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/yuxhuang/r-orch/compare/v0.13.0...v1.0.0
```

```
$ cargo xtask changelog assemble --version 1.0.1 --date 2026-12-01
changelog: assembled 1.0.1 (2026-12-01): 1 entry (Fixed 1); existing Unreleased text: none
changelog: wrote CHANGELOG.md
changelog: removed docs/changelog/unreleased/socket-timeout.fixed.md
```

`CHANGELOG.md` after:

```
## [Unreleased]

## [1.0.1] - 2026-12-01

### Fixed

- Fix the client socket timeout so a slow daemon reply no longer ends a command early.

## [1.0.0] - 2026-11-14
...
[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/yuxhuang/r-orch/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/yuxhuang/r-orch/compare/v0.13.0...v1.0.0
```

The PR that carried the fragment never named its own number, nothing edited `CHANGELOG.md` before the release PR,
and no follow-up PR was needed.

## Non-goals

No conversion of old notes; no network access, GitHub API or `gh` use by the tool; no new dependency (the
version, the dates and the small Markdown edits are hand-parsed); no change to the GitHub release step, the tag
or the Homebrew workflow; no generated Linux-only statement (Release step 4 says the release PR adds it by hand).

## Constraints

Security checklist (the tool reads files that arrive in a PR):
- Default bind and exposure: none; a local command.
- Bounds on input size, count and rate: R3 (1,000 bytes a fragment), R4 (500 fragments), 8 MiB for `CHANGELOG.md`.
- Permissions of files it writes: `CHANGELOG.md` keeps its existing mode; the temporary file is created
  exclusively (it never follows or overwrites an existing path) and removed on failure.
- Secrets and environment passed to children: none; no child process.
- Untrusted text rendered to a terminal or passed to a shell: fragment text is printed only after R3 refuses
  control characters; it is never passed to a shell.
- Paths: every fragment path is built from the validated name inside the one directory; the directory entry is
  checked without following symlinks; nothing outside `docs/changelog/unreleased/` and `CHANGELOG.md` is read or
  written, and `Cargo.toml` is only read.

## Verification environment

Everything runs on Linux, locally and in CI. The tests use a temporary directory and never the real
`CHANGELOG.md`. No owner hardware is needed.

## Open questions

- R14: CI would need a one-line workflow change (owner merges workflow changes). Recommended; without it a bad
  fragment is found only when the release PR runs `assemble`, which refuses and names the file.
- The tool carries an unmerged legacy `### Added` as is (R8). Alternative: merge same-named headings. Rejected
  for now: it needs rules for non-standard headings and rewrites text a reviewer wrote.

## Verification

Filled in by the PR that completes the work.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 to R16 | | | |
