# Replacing and patching definitions from files

`orch agent`, `orch task` and `orch config` each have an `edit` command that opens `$EDITOR`. For scripts, reviews and
version-controlled definitions they also have two commands that read a JSON or YAML document instead:

| Command | Does |
|---|---|
| `orch agent update <id> -f FILE` / `orch task update <id> -f FILE` / `orch config update -f FILE` | **Replace** the writable definition with the document. Omitted optional fields are cleared. |
| `orch agent patch <id> -f FILE` / `orch task patch <id> -f FILE` / `orch config patch -f FILE` | **Merge** the document into the current definition ([RFC 7396 JSON Merge Patch](https://www.rfc-editor.org/rfc/rfc7396.html)). |

The `edit` commands are unchanged. Goals, teams, context and messages have no file mutation. Runnable inputs are in
[examples/file-mutations/](../examples/file-mutations/).

```bash
orch agent update <agent-id> -f agent-update.yaml
echo '{"config": {"model": null}}' | orch agent patch <agent-id> -f -
orch --yaml config patch -f config-patch.yaml       # print the result as YAML
```

`<id>` is the entity id (`agt_…`, `tsk_…`), not an agent name.

## Input

- `-f/--file FILE_OR_DASH` is required; `-` reads stdin. At most 1 MiB, one document, nesting depth at most 64.
- **Language.** `--input-format json|yaml` wins. Otherwise `.json` is JSON, `.yml`/`.yaml` is YAML, and anything else
  (including stdin) is JSON if its first non-blank byte is `{` or `[` and YAML otherwise. A parse failure is final:
  there is no retry in the other language. The output flags `--json`/`--yaml` never choose the input language.
- The root must be an object. Duplicate keys, YAML tags, several YAML documents, empty input and a directory are
  rejected before anything is written.
- **Output.** Human mode prints `✓ Updated agent <name> (<id>)`, `✓ Updated task <id>` or `✓ Updated project
  configuration`; `--quiet` prints just the id for an agent or task (config keeps its success line); `--json`/`--yaml`
  print the saved entity (config: the saved raw configuration) and win over `--quiet`. Output is written only after
  the save succeeded.

## Replace (`update`) and merge (`patch`)

`update` takes the whole writable definition: a required field that is missing fails, an optional one that is
missing is cleared. `patch` applies the document to the current definition: an object member merges recursively, an
array replaces the whole array, and `null` **removes** the member (it never stores a null). A removed member that
has a default shows the default again. A merge that removes everything inside an object leaves `{}`:
`{"plugin": {"x": null}}` turns `plugin: {x: 1}` into `plugin: {}`. Both then run the same validation on the result.
Unknown fields are rejected, including unknown fields set to `null`. `null` in an `update` document is
rejected for agents and tasks (`agent.role: null is not allowed; omit the field`): omit the field instead.

| Entity | Writable (replace) | Never writable |
|---|---|---|
| Agent | `name`, `adapter`, `config` (required); `role` (optional). `config` keys: `command`, `model`, `effort`, `approval_policy`, `max_turns`, `timeout_ms`, `stall_timeout_ms`, `env`, `system_prompt`, `workspace_mode`, `skills`, `prompt_mode`, `role_summary` | `id`, `status`, `stats`, `capabilities`, `current_task`, `last_error`, `autonomous`, `remote` |
| Task | `title`, `description` (strings, required), `priority` (integer 1-4, required), `labels` (optional, default empty) | `id`, `status`, `assignee`, `depends_on`, `created_at`, `updated_at`, `attempts`, `max_attempts`, `scope`, `attachments` and the other runtime fields (reassign with `task assign`) |
| Config | The whole project `config.yml` object: `project`, `defaults` and `scheduling` with their required fields; any other key is kept | Global preferences (`orch config global`) |

- **Read-only fields.** `orch agent show`/`task show` output can be fed back to `update` unchanged: read-only fields
  whose value equals the current one are ignored; a different value fails (`<stdin>: stats: read-only field`). In a
  `patch` any read-only field fails, even when equal.
- **Reserved labels.** Labels starting with `remote-ctl:` or `exec:` belong to worker mirrors and are rejected;
  `remote-ctlx` and `exec-ish` are accepted.
- **Agents.** A shell agent still needs `config.command`; the name must not belong to another agent; the runtime
  limits must be positive; a shell agent's `model` is dropped, and so is its effort when it is `low`, `medium` or
  `high` (with a `legacy-ignored` warning); any other effort is refused. An agent that is running, or a remote
  one (an id containing `@`), cannot be changed.
- **Tasks.** A task that is `in_progress`, or labelled `remote-ctl:…`, cannot be changed. An update that changes nothing
  does not rewrite the task or touch `updated_at`.
- **Config.** Unknown keys survive both commands. The saved file is the raw candidate, not a typed round trip. Values
  are validated (positive concurrency and limits, objects where objects are expected). A `patch` that changes nothing
  writes nothing. The saved file keeps only what was written: after `patch` removed a default-backed key (for
  example `scheduling.max_concurrent_agents: null`), the file no longer contains it, so feeding `--json config patch`
  output back to `config update` fails with `required field` until the key is written out again.

## Errors

A failure changes nothing: parsing, merging and validation finish before anything is written. Validation errors exit
2 and name the source and field; not-found and I/O errors exit 1.

| Message (stderr) | Exit | Cause |
|---|---|---|
| `<stdin>: id: read-only field` | 2 | A read-only field in a patch, or a changed one in an update |
| `<stdin>: config.typo: unknown field` | 2 | Unknown field, even set to `null` |
| `<stdin>: title: required string` | 2 | A required field is missing or `null` |
| `<stdin>: priority: required integer between 1 and 4` | 2 | Out-of-range or non-integer priority |
| `<stdin>: labels: remote-ctl: and exec: prefixes are reserved for worker mirrors` | 2 | Reserved label |
| `<stdin>: agent.role: null is not allowed; omit the field` | 2 | `null` in an `update` document |
| `<stdin>: name: another agent already has this name` | 2 | Duplicate agent name |
| `<file>: Shell agents require a command` | 2 | A shell agent without `config.command` |
| `<stdin>: config: runtime limits and concurrency must be positive` | 2 | A zero limit or concurrency in a config document |
| `<stdin>: config.defaults: required field` | 2 | A config `update` document that omits a required block |
| `<stdin>: invalid JSON (check types, duplicate keys and depth <= 64)` | 2 | Malformed JSON, a duplicate key or too deep |
| `<stdin>: document root: expected an object` | 2 | Empty input, or a root that is a list or scalar |
| `<stdin>: explicit YAML tags are not allowed` / `expected exactly one YAML document` | 2 | Tag or several documents |
| `<file>: document exceeds 1 MiB` / `expected a regular input file` | 2 | Size limit; a directory |
| `cannot mutate a running or remote agent definition` (task: `… remote-controlled task …`) | 2 | Running or remote target (checked in the service; covered by its unit tests) |
| `Agent not found: <id>` | 1 | Unknown id; names are not accepted |
| `<stdin>: mutation lock timed out after 5 s; retry` | 1 | Another mutation held the lock for 5 s |
| `read mutation input` | 1 | The file cannot be read. The message names the operation only, not the path or the OS reason |

## Concurrency and limits

- The three commands share one advisory lock, `.orchestry/.definition.lock`, taken for up to 5 seconds. It serializes
  these commands against each other, so two patches to different fields both survive.
- It does **not** cover `edit`, `agent add`, a running orchestrator or the dashboard. Those keep their existing
  last-writer-wins behaviour, and an agent or task is saved through its normal store, which is not one transaction with
  its index. To change many definitions reproducibly, stop the runner and use one writer.
- The write itself is atomic (a temporary file renamed over the target). A crash after the rename but before the
  output was printed has still saved the change: run `orch agent show`/`task show`/`config get` before retrying.
- Rolling back is a second `update` with the previous definition. Save `orch --yaml agent show <id>` first.
