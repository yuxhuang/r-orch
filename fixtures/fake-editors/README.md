# Fake editors

`task edit` and `agent edit` take no flags: they serialize the entity to a temp
file, spawn `$EDITOR` (then `$VISUAL`, then `vi`) on it, and parse whatever
comes back. That makes them the only two subcommands in PARITY §2.2 that are
unreachable from a scenario unless `$EDITOR` is something scriptable.

These are that something — non-interactive filters over the frontmatter format
in `src/cli/editor.ts`. A scenario puts this directory first on `PATH` with
`fake_editor = "<name>"`, which also sets `EDITOR` to that name, so both
binaries get byte-identical edits and the comparison is about the *edit
protocol*: the temp-file round trip, which fields a changed line updates, and
what a non-zero editor exit does.

| Script | What it does |
|---|---|
| `orch-edit-title` | Rewrites `title:` in the frontmatter, leaves the body alone |
| `orch-edit-body` | Replaces the description/role body, leaves the frontmatter alone |
| `orch-edit-noop` | Writes nothing back — exercises the "No changes." path |
| `orch-edit-fail` | Exits 1 without writing — the editor-failed error path |

Each takes the temp file as its only argument, as a real editor does.
