# CLI test map

Where `orch-cli` behaviour is tested. Keep this current.

| Area | Cases | Rust home |
|---|---|---|
| Routing | project discovery, daemon routing, help fast path | `src/routing.rs` (unit) + `tests/routing.rs` (through the binary) |
| Context | `NO_COLOR` presence, `TERM=dumb`, project-root walk | `src/context.rs` |
| Output | icons, colours, table padding, formatters | `src/output.rs` |
| `init` | validation, idempotency, files written, adapter detection, the multi-adapter prompt | `src/commands/init.rs` + `tests/routing.rs` |
| `doctor` | daemon report rendering; discovery-only failure without a daemon | `src/doctor.rs`, `src/commands/run.rs`, `tests/routing.rs`, `tests/offline_doctor_contract.rs` |
| `logs` | five routes, fetch sizes, display slices, four `--json` payloads, `--follow` shapes | `src/logs.rs` + `tests/routing.rs` |
| `status` | human block, `{tasks, agents, state}`, `claimed` as an **array** | `src/commands/status.rs` + `tests/routing.rs` |
| `logs --since` | the `--since`-only route, its run window and its 20-run cap | `src/logs.rs` |
| `serve` options | `--log-format` validation and `--log-file` append for `--once`; refusal (exit 2) of `--tick-interval`, plus `--log-file`, `--verbose` and non-default `--log-format` on a foreground start; explicit foreground daemon start | `src/serve/mod.rs`, `src/commands/daemon.rs`, `tests/routing.rs` |
| `serve --once` | daemon batch request, terminal outcome, empty project and observer disconnect | `src/commands/run.rs`, `tests/serve_interrupted.rs`, `../orch-daemon/tests/daemon_dispatch.rs` |
| Structured logger | record order, idle throttle, `agent:output` gating, the never-logged set, text format | `src/serve/logger.rs` |
| Dashboard push | subscribe before read, store/bus delivery, commit ordinals, gap/resync, changed daemon instance, EOF and retry | `src/tui/push_tests.rs` |
| Dashboard daemon facade | typed backend operations, dispatch controls and daemon-backed reads | `tests/tui_backend.rs` |
| No daemon / no auto-start | absent, stale and skewed endpoints across clients; no spawn or endpoint repair | `tests/daemon_nodaemon_matrix.rs`, `tests/daemon_lifecycle_nospawn.rs` |
| Task editor | buffer round trip, frontmatter parsing | `src/editor.rs` |
| Agent editor | buffer, comment stripping, role-always-set | `src/editor.rs` |

## Not tested

There is no installer script and no npm lifecycle hook, so there are no install-time tests.

## Notes on `serve` and `run --watch`

`run --watch` and `serve` without `--once` explicitly start a foreground daemon with dispatch on.
`tests/watch_shutdown.rs` exercises their signal shutdown. `serve --once` instead requests a batch
from a running daemon; `tests/serve_interrupted.rs` checks interruption of that client observation. The `heap_mb` value of
`orchestrator:tick` records and clock times in `run`/`logs` output vary between runs, so tests do not assert
exact values for them.
