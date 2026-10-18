# r-orch operator runbook

Recovery and gotcha reference for running r-orch. Everything here is **inherited behaviour replicated
deliberately** from the TypeScript original unless marked otherwise — the parity contract
([PARITY.md](PARITY.md)) explains why each was kept rather than fixed, and each entry below names its section.

If you are looking for *what r-orch is* and how to build it, start at the [README](../README.md).

---

## Quick index

| Symptom | Go to |
|---|---|
| `Orchestrator already running (PID: undefined)` and nothing is running | [Corrupt lock file](#corrupt-lock-file) |
| `serve --once` never exits | [`serve --once` hangs on review](#serve---once-hangs-on-a-task-in-review) |
| `task show <id>` works but `task list` omits the task | [Rebuilding an index](#rebuilding-an-index-after-a-lost-update) |
| Every command fails after a crash or power loss | [state.json recovery](#statejson-parse-failure) |
| `.state.json.*.tmp` files piling up | [Temp-file litter](#state-file-temp-litter) |
| `serve:started` logged but the daemon is not up | [serve:started ordering](#servestarted-precedes-the-lock-check) |
| Need to see what r-orch is doing internally | [Debugging](#debugging) |
| Agent processes still running after a cancel | [Stray agent subprocesses](#stray-agent-subprocesses-after-a-cancel) |
| An agent has run for hours and nothing stopped it | [`timeout_ms` is not a timeout](#timeout_ms-is-not-a-timeout) |
| Running r-orch from inside a Claude Code session | [Nested agent sessions](#nested-agent-sessions) |
| Worried about API keys in agent config | [Secrets in agent config](#secrets-in-agent-config) |
| Cannot select text in the dashboard any more | [Mouse capture holds the pointer](#mouse-capture-holds-the-pointer) |
| The terminal prints `[<0;44;8M` when you click | [A leaked mouse mode](#a-leaked-mouse-mode-after-an-abnormal-exit) |
| A task handed to a peer shows `dispatched`, `queued` or `RUN?` | [Remote task phases](#remote-task-phases) |
| A peer is `offline` and you cannot tell why | [Reading a peer's link state](#reading-a-peers-link-state) |
| A mirror on a worker sits `todo` for ever | [Mirrors left behind on a worker](#mirrors-left-behind-on-a-worker) |
| Changing the peer protocol | [Peer protocol compatibility](#peer-protocol-compatibility) |

## Structured output for `doctor`, `logs --follow` and `serve`

**Symptom.** `orch --json doctor` (or `--yaml doctor`) piped into a parser fails on the first line;
`orch --yaml logs --follow` or `orch --yaml serve` prints text or JSON Lines, not YAML.

**Cause.**
- `doctor` prints its human header (`  orch doctor · checking adapters and dependencies`) to **stdout** before the
  structured document, whichever of `--json` or `--yaml` is given. The header is older than `--yaml` (master
  prints it under `--json` too), and `--yaml` does not change it. Outside a project, `doctor` ignores both flags
  and prints the text report.
- `logs --follow` prints a text banner and then live events; `serve` writes JSON Lines (`--log-format json`, the
  default) or text. `--yaml` changes neither, and it never changes the on-disk `.jsonl` logs.
- A failing command prints its error as text on stderr under every output flag; stdout is empty.

**Fix.** For `doctor`, drop the leading header lines before parsing: `orch --json doctor | sed '1,2d'`. For
streams, parse the JSON Lines `serve` writes, or poll `orch --yaml logs --task <id>` for run metadata. To tell an
error from output, check the exit code; never parse stderr as YAML or JSON.
