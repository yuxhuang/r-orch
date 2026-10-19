# `fixtures/fake-agents/` — stand-in agent CLIs

Seven executables named exactly like the real CLIs the adapters spawn:

| script | stands in for | protocol |
|---|---|---|
| `claude` | Claude Code | `--print --output-format stream-json`, JSONL on stdout |
| `cursor-agent` | Cursor Agent | the same stream-json shapes |
| `codex` | Codex | `exec --json -`, prompt on **stdin**, JSONL out |
| `opencode` | OpenCode | `run --format json`, JSONL out |
| `pi` | pi-coding-agent | `--mode rpc`, one JSONL frame in, RPC events out, session stays alive |
| `grok` | Grok | `-p <prompt> --output-format streaming-json` |
| `agy` | Google Antigravity | `-p <prompt>`, plain text out |

Tests put this directory **first on `PATH`**, so a run is reproducible and
hermetic: POSIX `sh` only, no `node`, no `npm`, no network. `--version` short-circuits every script, so `orch doctor`
probes cleanly.

There is no fake for the `shell` adapter: it has no CLI of its own and runs
whatever `agent.config.command` says.

## Driving them

Each script picks a scenario from `ORCH_FAKE_SCENARIO`, which a marker
anywhere in the **prompt** overrides — that is how one scenario makes a single
task behave differently, with nothing but the rendered prompt.

| scenario | prompt marker | behaviour |
|---|---|---|
| `success` (default) | — | one assistant message, then the adapter's terminal event |
| `fail` | `ORCH_FAKE_FAIL` | a provider error and exit 1, with `fake-agent: the task failed` on stderr |
| `rate_limit` | `ORCH_FAKE_RATE_LIMIT` | as `fail`, but worded so `classifyAdapterError` returns `rate_limit` |
| `stderr_flood` | `ORCH_FAKE_STDERR_FLOOD` | 256 KiB on stderr *before* any stdout — deadlocks an adapter that fails to drain it |
| `slow` | `ORCH_FAKE_SLOW` | sleeps `ORCH_FAKE_SLOW_SECONDS` (default 30) before emitting, for stall detection |
| `crash` | `ORCH_FAKE_CRASH` | `kill -9` on itself: exit code null, killed by a signal |
| `silent` | `ORCH_FAKE_SILENT` | terminal event only, no output events |

Other knobs: `ORCH_FAKE_REPLY` (the reply text, default `ORCH_FAKE_OK`),
`ORCH_FAKE_VERSION`, `ORCH_FAKE_EXIT_CODE`.

## Keeping them honest

`crates/orch-infra/tests/fake_agents.rs` runs every real adapter against its
fake and asserts the full event sequence, the version probe, the failure
scenarios and the stderr flood. A script that drifts into a shape an adapter
mis-parses fails there rather than silently corrupting every scenario built on
it.
