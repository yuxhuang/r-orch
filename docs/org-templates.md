# Organization templates

An organization template is a directory that describes a team: one `org.yml`, one YAML file per member and optional
shared rule files. `orch org deploy` turns it into agents, a team and, if you ask, a goal. `orch agent shop` can
install a single member from it. `orch org template …` lists, inspects, creates and changes templates without an
editor. The embedded **a-team** template (27 specialist roles) is built in; the ten older presets keep working.

Contents: [Quick start](#quick-start) · [Directory format](#directory-format) · [Rules](#shared-rules) ·
[Which template is used](#which-template-is-used) · [Commands](#commands) · [Deploying](#deploying) ·
[The agent shop](#the-agent-shop) · [Changing a template](#changing-a-template) ·
[Repairing a broken template](#repairing-a-broken-template) · [Validation errors](#validation-errors) ·
[Concurrency and limits](#concurrency-and-limits) · [The a-team](#the-a-team) · [Not covered](#not-covered)

Runnable inputs are in [examples/org-templates/](../examples/org-templates/). Design record:
[ADR 0006](decisions/0006-org-templates.md) and [the plan](plans/2026-10-17-org-templates.md).

## Quick start

No project is needed to list, show, create or change a template. Deploying and the shop need one (`orch init`).

```bash
orch org template list                                   # embedded, legacy and your own templates
orch org template show a-team --resolved                 # what deploy would create, field by field
orch org deploy --template-dir ./review-pair --goal "Review the open PR"
orch agent shop --template-dir ./review-pair             # pick one member to install
```

Template commands choose a template in one of two ways: a **name** (`review-pair`, `builtin:a-team`) looked up in
your user template root, or `--template-dir PATH` for an exact directory. They are mutually exclusive, and one of
them is required. The mutation commands take the member with `--agent NAME` or as a second positional argument:
`orch org template agent patch review-pair --agent reviewer -f x.json` and `… patch review-pair reviewer -f x.json`
are the same command.

## Directory format

```
review-pair/
  org.yml                 required
  agents/<name>.yml       required directory, 1 to 128 members
  rules/<id>.md           optional; one file per rule listed in org.yml
```

**Identifiers** (template name, lead, member names and file stems, rule ids) are lowercase ASCII slugs: a letter
first, then letters, digits and single hyphens, at most 64 bytes, no empty segment (`a--b`, `-a`, `a-`), and not a
Windows device name (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`).

**Files.** Each file is at most 1 MiB of UTF-8 and the whole template at most 16 MiB. A member file must be named
`<name>.yml` and its `name` field must equal that stem (`.yaml` is rejected with a rename hint). Files in `agents/`
and `rules/` with another extension are ignored. A directory or symlink there, or a rule file that `org.yml` does not
list, is an error. Names starting with `.` are skipped entirely, whatever they are ([see below](#dot-entries-and-links)).

**YAML.** One document per file, JSON-compatible values only, no explicit tags, duplicate keys rejected, nesting and
node counts bounded, unknown keys rejected (the error lists the valid ones).

### `org.yml`

```yaml
schema_version: 1            # required; only 1 is supported
name: review-pair            # required; for a user template it must equal the directory name
lead: lead                   # required; must be one of the members
description: A lead who plans and a reviewer who checks the diff
rules: [house-style]         # the rule registry: every rules/<id>.md, in the order the sections are embedded
defaults:                    # optional; applies to every member that does not say otherwise
  adapter: claude
  approval_policy: auto
  max_turns: 40
```

### `agents/<name>.yml`

```yaml
name: reviewer               # required; equals the file stem
description: Reviews the diff and reports findings
adapter: codex               # optional, like every field below
rules: [house-style]         # rules this member embeds
skills: []                   # [] means "none", which is different from leaving the key out
role: |                      # required, not blank
  Code reviewer. Read the diff, run the checks and report each finding with its file and line.
```

The optional fields, in `defaults` and in a member, are `adapter` (`claude`, `opencode`, `codex`, `cursor`, `pi`,
`grok`, `antigravity` or `shell`), `model`, `command` (required when the final adapter is `shell`), `effort`
(`low`, `medium`, `high`), `approval_policy` (`suggest`, `auto`, `manual`), `max_turns`, `timeout_ms`,
`stall_timeout_ms` (positive integers), `env`, `system_prompt`, `workspace_mode` (`shared`, `worktree`, `isolated`)
and `skills`. A blank `model` is refused. A shell agent drops `model` and `effort`.

### Which value wins

For each member, `orch org template show NAME --resolved` prints exactly what `deploy` and the shop will create.

| Field | Order, first match wins |
|---|---|
| `adapter` | `--adapter`, the member, `defaults.adapter`, the project default |
| `model` | `--model`; the member's `model`, but only if the member's own adapter (its `adapter`, else `defaults.adapter`) is the final adapter; `defaults.model`, on the same condition; else none |
| `approval_policy`, `max_turns`, `timeout_ms`, `stall_timeout_ms` | the member, `defaults`, the project default (`config.yml`) |
| everything else | the member, `defaults` |
| `skills` | see below |

Models are bound to an adapter, so `--adapter codex` drops a model that was written for `claude`. With `--adapter
codex`, a member whose own adapter is already `codex` keeps its model. `--adapter` and `--model` apply to every member.

**`skills` and the other optional fields keep three states.** Omitted means "inherit": the member gets
`defaults.skills`, or none if there are none. Present and empty (`skills: []`) means "override with nothing". Present
and non-empty replaces the defaults, it does not merge with them. `show` without `--resolved` prints only what is
written, never the inherited defaults. In a **patch**, `null` removes the key (back to inherit). In a **definition**
(an `update` document or a file) `null` is rejected: leave the key out.

## Shared rules

A rule is a Markdown file registered in `org.yml`. A member embeds it by listing its id in `rules:`, or by naming
the file in its role text with the canonical reference `.claude/agent-rules/<id>.md`. A rule may name other rules the
same way, and the whole closure is embedded. Nothing is read from disk when the agent runs.

A role with a **non-empty closure** is deployed as

```
Embedded shared-rule sections are authoritative; no rule-file lookup is required.[ <addendum>]

<role text, with each canonical reference rewritten to: embedded rule «id»>

## Shared rule: <id>

<rule text>
…one section per rule in the closure, in the order of org.yml's rules list
```

The sentence "Rules (relative to your workspace root):" in a role is rewritten to "Rules (embedded in this role):".
The `<addendum>` is added only when the closure is non-empty **and** at least one of these holds: the template is
named `a-team`; the role text contains the literal string `<workspace root>`; a rule in the closure contains it.
Its exact text is:

> `<workspace root>` means the owning orch project root (the directory containing .orchestry), not the per-task
> checkout. Establish it from task/context before using destination placeholders. If unavailable, report it to the
> lead instead of guessing from cwd. Sibling ../pensieve destinations and named skills are deployment prerequisites;
> edit the template for other layouts.

A role with an **empty closure** is deployed byte for byte as written, even if it contains `<workspace root>`.

Checked at load time: a reference to a rule that is not registered, a malformed reference, and the shorthand
`seniority.md` (or any registered id plus `.md`) without the `.claude/agent-rules/` prefix are errors, because the
shorthand is ambiguous. The deployed agent keeps no link to the template: later edits to the template never change an
agent that already exists.

## Which template is used

A **name** is `KEY`, `builtin:KEY` or `user:KEY`.

| Written | Resolves to |
|---|---|
| `KEY` | your user template `KEY` if the directory exists (even if it is invalid, which is then an error); otherwise the embedded `a-team`; otherwise the legacy preset `KEY` |
| `builtin:KEY` | the embedded `a-team` or a legacy preset, ignoring any user template of that name |
| `user:KEY` | your user template only |
| `--template-dir PATH` | exactly that directory; no lookup, and its `name` need not match the directory |

The user template root is `templates/` beside the global config file: `~/.orchestry/templates/` by default, or
beside the file named by `ORCHESTRY_GLOBAL_CONFIG_PATH`. A user template named like a builtin shadows it:
`orch org template list` marks the builtin row `shadowed`.

**Builtins are read-only.** Any mutation of `a-team` or a legacy preset (without a user template of that name)
fails with `Builtins are read-only; run orch org template init a-team-copy --from builtin:a-team`. Copy it, then
change the copy. The ten legacy presets (`startup-mvp`, `pr-review-corp`, …) can be shown and copied this way too.

## Commands

| Command | Does |
|---|---|
| `orch org template list` | One row per template: `NAME`, `SOURCE` (`builtin`, `legacy`, `user`) and `STATUS` (`ok`, `shadowed`, or the reason an invalid user template fails). Exit 1 if any entry is invalid. `--quiet` prints names only. |
| `orch org template show [NAME] [--template-dir PATH] [--resolved]` | The source and the parsed template. `--resolved` adds `resolved_agents`, the exact agent inputs. |
| `orch org template init NAME [--from SOURCE] [--template-dir PATH]` | Create a template (a minimal one with a lead, or a copy of `SOURCE`, rules included). Never overwrites: an existing destination is an error. The copy gets the new name. |
| `orch org template edit [NAME]` | Open `org.yml` in `$EDITOR` (then `$VISUAL`) on a temporary copy; validate; publish. |
| `orch org template update [NAME] -f FILE` | Replace `org.yml` with a complete JSON or YAML definition. |
| `orch org template patch [NAME] -f FILE` | Merge a partial definition into `org.yml`. |
| `orch org template agent list [NAME]` | The members as authored. |
| `orch org template agent add [NAME] -f FILE` | Create `agents/<name>.yml` from a definition; refused if it exists. |
| `orch org template agent edit\|update\|patch [NAME] [AGENT]` | Edit, replace or merge-patch one member (`--agent AGENT` or positional). |
| `orch org template agent delete [NAME] [AGENT]` | Remove a member. The remainder is validated, so the lead cannot be deleted. |
| `orch org deploy KEY` · `--template NAME` · `--template-dir PATH` | Deploy ([below](#deploying)). |
| `orch agent shop [--template NAME \| --template-dir PATH] [--list]` | Install one member ([below](#the-agent-shop)). |

Every command takes `--json` or `--yaml`. Without either, `list` prints a table and the others print YAML: `init`
prints `name` and `path`, `delete` prints `deleted: NAME`, and the rest print the resulting definition. None of the
`org template` commands needs a project (`show --resolved` uses the project's defaults when there is one).

`-f FILE_OR_DASH` reads a file or stdin. The language is `--input-format json|yaml` if given; otherwise `.json` is
JSON, `.yml` and `.yaml` are YAML, and anything else, stdin included, is JSON when the first non-blank byte is `{`
or `[` and YAML otherwise. A parse failure is final: there is no retry in the other language. `--json` and `--yaml`
choose the output only. The document must be a single object of at most 1 MiB.

## Deploying

```
orch org deploy KEY                      orch org deploy --template NAME
orch org deploy --template-dir PATH      [--adapter A] [--model M] [--goal "TITLE"]
```

`KEY` alone, with no `--adapter`/`--model`, no `:` and no user template of that name, runs the **legacy** route for
the ten old presets and prints the old human summary. Everything else, `a-team` included, runs the template route,
which prints `team`, `agentIds` and `goalId` (`--json`, `--yaml`; YAML by default) or just the team id with
`--quiet`. Combining `KEY` with `--template` or `--template-dir` is a usage error.

Before anything is created the template route validates the whole template, resolves every member and checks the
rendered record size, and refuses a duplicate agent name (`Agent "lead" already exists; no agents created`) or a blank
goal. Then, in this order, it creates the members (sorted by name), a team named after the template whose lead is
`lead` from `org.yml`, and the goal if requested, assigned to the lead.

### When a deploy fails half way

Deployment goes through the ordinary services and **is not transactional**. If a step fails after others succeeded,
nothing is rolled back and the error names every survivor and the stage:

```
✕ Template deployment failed at goal: create …/.orchestry/goals. Surviving entities:
  {"agentIds":["agt_…","agt_…"],"teamId":"team_…","goalId":null,"unverifiedIds":["goal_…"]}. Inspect the entity files …
```

`unverifiedIds` are ids that were allocated but whose existence could not be checked. To recover, fix the cause, then
remove the survivors yourself:

```bash
orch agent remove <agt_…>          # each survivor
orch team disband <team_…>
orch goal delete <goal_…>          # if one was created
```

**A disbanded team still owns its name, and deploy does not check team names in advance.** Redeploying the same
template creates the members first and then fails at the team stage with `Team "review-pair" already exists`, leaving
new survivors. Before redeploying, either copy the template under another name (`orch org template init other --from
user:review-pair`), or, as a manual repair that is not a supported interface, delete the leftover record
`.orchestry/teams/<team_…>.yml` (teams have no index file in this version). Both were run in a disposable project.

## The agent shop

`orch agent shop` with no option is the built-in catalogue of 15 role templates and is unchanged. With
`--template NAME` or `--template-dir PATH` it lists the **members of that template** and installs one, using the same
resolution as `deploy`: the same adapter, model, skills and embedded rules, byte for byte.

- `--list`, or stdout or stdin not being a terminal, prints a table (`NAME`, `DESCRIPTION`, `ADAPTER/MODEL`, `SKILLS`).
  `--json` and `--yaml` print the members as authored instead.
- In a terminal it draws the picker: `↑`/`↓` move, `Enter` installs the highlighted member, `q`, `Esc` and `Ctrl-C`
  cancel. Cancelling writes nothing and exits 0. Installing prints `✓ Added agent reviewer → agt_…`.
- An existing agent name is refused with exit 2 and nothing is written. The message goes to stderr; in the tmux
  captures it was visible on screen in only one of seven runs, so check the exit status or `orch agent list`.
- **Resize and a key arriving together.** The picker reads the terminal's current size before drawing every frame, so
  a key that arrives with a resize, with no separate resize event, still gets a frame that fits. In-repo pty tests
  cover a shrink and a grow delivered back to back with a key. In tmux, resizing an 80x24 terminal to 50x9 in the same
  command as `Down` showed the second row selected at the new size, and `Enter` installed it. That run is a check of
  the behaviour, not a before-and-after measurement of the original fault (it was not reproduced in tmux).
- The terminal is restored when the picker exits, whether by choice, cancel or refusal (`icanon`, `echo` and `opost`
  were set again in every run).
- At widths of about 40 columns or less the heading is cut with an ellipsis and may not show `q cancel`; the keys
  still work. Keeping the cancel hint visible on narrow terminals is a candidate change that has not been accepted.

## Changing a template

`update` and `patch` do the same job as `edit`, from a file, with no editor. Each changes **one file**: `org.yml` or
one member.

|  | `update` | `patch` |
|---|---|---|
| Document | the complete definition | any part of it |
| Omitted optional field | cleared | kept |
| `null` | rejected (`definition.skills: null is not allowed; omit the field`) | removes the key |
| Objects | replaced | merged key by key |
| Arrays (`rules`, `skills`) | replaced | replaced whole |
| Needs a parseable current file | no ([repair](#repairing-a-broken-template)) | yes |

`patch` is an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) merge patch. An empty patch (`{}`) writes
nothing. An update or patch that changes nothing also writes nothing (the file's content and mtime stay).

```bash
orch org template update review-pair -f org-update.json
orch org template patch  review-pair -f org-patch.yaml        # null under defaults removes a default
orch org template agent update review-pair --agent reviewer -f agent-update.yaml
echo '{"skills": null}' | orch org template agent patch review-pair reviewer -f -
```

What is allowed:

- `org.yml` accepts `schema_version`, `name`, `lead`, `description`, `rules` and `defaults` (and inside it only the
  twelve optional fields). A member accepts `name`, `role`, `description`, `rules` and the same twelve fields. Any
  other key, even set to `null`, fails: `<stdin>: colour: unknown field`.
- **Immutable:** `name` and `schema_version` of `org.yml` (`name is immutable`; for a user template also `org.yml name
  must match the user directory key`), and a member's `name` (`name must equal filename stem`). To rename a template,
  copy it with `init --from`.
- The `lead` must name an existing member (`Lead ghost is missing`), so the lead cannot be deleted either.
- Rule references are checked on every change.
- The whole candidate template is validated before anything is written, **including files you did not touch**. A
  rejected command leaves the tree byte-identical.

Results are printed as the typed document, so a rewritten file loses its comments and original layout. Files written
by `init` and by these commands are owner-read/write only.

## Repairing a broken template

A hand edit can leave a template that no longer loads. `list` shows the reason; `show`, `deploy` and the shop fail
with it.

- **`update` repairs one broken file.** It overlays the new bytes for that file (the old one must still be a readable
  file of at most 1 MiB: a larger file makes every command on that template fail with `exceeds 1 MiB`, `update`
  included, so shrink or delete it by hand) and then validates everything. If any *other* file is also broken, the update is refused and
  names that file.
- **`patch`, `edit` and `agent add`/`delete` need a parseable base.** `patch` on a broken file says
  `…; use template update with a full valid definition to repair`. `agent delete` of a broken member works if what
  remains is valid.
- **Root identity.** If `org.yml` parses but is **missing** `name` or `schema_version`, a full `update` supplies it
  (the name must still match a user template's directory, and the schema must be `1`). If either is **present**,
  including `null` or an unsupported version such as `2`, it cannot be changed: `name is immutable`,
  `schema_version is immutable`. Fix such a file by hand.

```text
$ orch org template update --template-dir ./broken -f org-update.json     # org.yml was "lead: ["
schema_version: 1
name: review-pair
…
```

## Validation errors

Every load error names the file and the reason. Examples, as printed (exit 2 unless noted):

```text
agents/lead.yml: explicit YAML tags are forbidden
agents/lead.yml: while parsing a flow sequence, expected ',' or ']' at byte 27 line 3 column 1
agents/lead.yml: unknown field `colour`, expected one of `name`, `role`, `description`, …
agents/other.yml: name must equal filename stem
<dir>/agents/extra.yaml: rename .yaml member to .yml
<dir>/agents/sub: nested or nonregular template entry
<dir>/agents/lead.yml: exceeds 1 MiB
agents/lead.yml: expected UTF-8
Lead lead is missing
Missing rules/house-style.md
Unregistered rule asset
rules/inner.md: Unregistered rule reference .claude/agent-rules/ghost.md
rules/inner.md: Ambiguous rule reference stance.md; use .claude/agent-rules/stance.md
Too many agents                      (more than 128; a rules/ directory with more than 64 files is refused too)
parse org.yml: duplicate entry with key "name"                  (exit 1)
parse org.yml: deserializing from YAML containing more than one document is not supported   (exit 1)
```

The last two exit 1, not 2; the rest of the parse and validation failures exit 2.

For `update`, `patch` and `add`, input errors are prefixed with the source, `<stdin>:` or the file name:
`<stdin>: invalid YAML (check duplicate keys and alias/node limits)`, `<stdin>: document root: expected an object`,
`<stdin>: max_turns must be positive`, `<stdin>: Unsupported adapter nope`.

### Dot entries and links

Inside `agents/` and `rules/`, any name starting with `.` is skipped and never opened or followed: a dot file, a dot
directory and a dot symlink (even a dangling one) all leave the template valid. A symlink or directory with an
ordinary name is rejected. The template root itself must be a real directory: `--template-dir link`, `link/` and
`link/.` are all refused with `expected directory without symlinks`, for reads and for writes, and a refused write does
not create a lock file in the target.

## Concurrency and limits

- Each mutation takes `.template.lock` in the template root for at most 5 seconds (`Timed out after 5 s locking …`
  exit 2), reads the files, validates the candidate, and writes the one target file atomically (a temporary file renamed
  over it; `add` refuses to replace an existing file). `init` builds a staging directory next to the destination and
  renames it, under `.template-init.lock` in the parent directory.
- Only commands that take the lock exclude each other. Hand edits, other tools and a concurrent `deploy` are not
  locked out: last writer wins, and a change made by another process of the same user between two commands is outside
  the guarantee.
- Reading does not lock. A reader can see a template between two of your commands, and a file that is replaced while it
  lists the directory is skipped for that read. **A read is not a transactional snapshot of several files**; do not rely
  on one for consistency across commands.
- A crash after the rename but before the output was printed has still saved the change; run `show` before retrying.
- Deployed agents, teams and goals are ordinary records; they keep the last-writer-wins behaviour of the rest of
  r-orch, and `deploy` is not transactional (above).
- This is not a defence against someone who can already write your template directory or your home directory.

## The a-team

`a-team` is embedded in the binary: 27 members, the lead `orchestrator`, and three shared rules (`seniority`,
`collaboration-protocol`, `stance`). It was extracted from an existing set of specialist agent roles: lead,
architect, engineers (including two `backend-eng` stubs whose whole role is the line `Backend developer`), reviewers,
security, QA, SRE, release, data, AI, performance, documentation, product and strategy roles. Fourteen members use the
`codex` adapter and thirteen use `claude`; none pins a model, so the adapter's default applies unless you pass
`--model`. Org defaults: `approval_policy: auto`, `max_turns: 100`, `timeout_ms: 3600000`, `stall_timeout_ms: 600000`.

```bash
orch org template list                      # a-team is listed as builtin
orch org template show a-team --resolved    # all 27 deployed roles
orch org deploy a-team --goal "Ship the release"
orch org template init my-team --from builtin:a-team   # a writable copy
```

Of the 27 roles, 25 embed shared rules (two with all three, the rest with two) and are deployed with the
[embedded-rule prefix and the workspace-root addendum](#shared-rules); the two `backend-eng` stubs have an empty
closure and are deployed unchanged. The role texts are the originals with personal directory information removed and
directories made relative to the workspace root; the `sec-reviewer-codex` role keeps its security addendum paragraph.
A search of the embedded roles and rules for `/Users/`, `/home/` and the extractor's user and organisation names
found nothing.

### Prerequisites

A deployed a-team assumes the following. `deploy` does not check any of it.

- The `claude` and `codex` agent CLIs are installed and signed in. `orch doctor` reports which adapters are ready.
- Roles name **skills** that your agent CLI must provide: `review`, `careful`, `qa`, `ship`, `canary`,
  `document-release`, `plan-eng-review`, `office-hours`, `design-review`, `fact driven` (with the space), and the
  plugin skills `feature-dev:feature-dev`, `feature-dev:code-explorer`, `feature-dev:code-reviewer`,
  `feature-dev:code-architect`, `testing-suite:generate-tests`, `testing-suite:test-coverage` and
  `devops-automation:cloud-architect`. `orch` does not install skills. A missing one is only noticed when the agent
  tries to use it. Edit the member file in a copy of the template to drop or rename a skill.
- **`<workspace root>`** in a role means the directory that contains `.orchestry/`, not the per-task checkout. The
  addendum tells each agent to establish it from the task or context and to report to the lead rather than guess from
  its working directory.
- Some destinations are written relative to that root as **`<workspace root>/../pensieve/…`** (for example
  `specs/` and `ateam-reports/` in the orchestrator and technical-writer roles) and as `<project root>/ateam-reports/`.
  The directory beside your project root must exist, or those roles must be edited for your layout.

## Not covered

- Removing a rule, or adding one, is a file operation plus an `org.yml` change; there is no `rule` subcommand.
- Template inheritance, replicas, remote registries, automatic skill installation and updating already-deployed
  agents from a template are not supported.
- Parsing is bounded and path-contained, but this guide makes **no general claim** that the YAML or JSON parsers are
  safe against every hostile input, and a read is not a transactional snapshot (see above).
- **Known limitation of this slice.** A valid JSON surrogate-pair escape (a backslash-u high surrogate followed by a
  backslash-u low surrogate, such as the escaped crab emoji) in a document given to `org template agent add` is rejected
  with `invalid Unicode character escape code`, although the same character written as literal UTF-8 is accepted, and
  `agent patch` and `agent update` accept the escape. A correction is a candidate that is still under independent
  review and is not part of this baseline; write literal UTF-8 for now.
