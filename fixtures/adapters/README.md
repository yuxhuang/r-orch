# `fixtures/adapters/` — recorded adapter streams (V15)

One directory per adapter kind. Each case is a pair:

| file | what it is |
|---|---|
| `<case>.jsonl` / `<case>.txt` | a recorded **stdout** stream, exactly as the CLI would write it |
| `<case>.expected.json` | the `AgentEvent` sequence it must produce, plus the terminal error if the run fails |

`crates/orch-infra/tests/adapter_fixtures.rs` replays each stream through the
real adapter (over a scripted child process, so the real pipe, line-splitting
and decoding path runs) and compares the result.

## Provenance

The stdout payloads are taken from the TypeScript suites, so the expectations
are anchored to the original rather than to this port:

| fixture | source |
|---|---|
| `claude/*` | `test/unit/infrastructure/claude-adapter.test.ts` |
| `codex/session` | `test/unit/infrastructure/codex-adapter.test.ts` |
| `codex/duplicate-error` | same file — the nested provider envelope and the model-metadata warning |
| `cursor/session` | `test/unit/infrastructure/cursor-adapter.test.ts` |
| `opencode/*` | `test/unit/infrastructure/opencode-adapter.test.ts` |
| `pi/session` | `test/integration/pi-adapter.e2e.test.ts` |
| `pi/auto-retry` | `test/unit/infrastructure/pi-adapter.test.ts` |
| `grok/session`, `antigravity/session` | `test/integration/new-adapters.e2e.test.ts` |
| `shell/session` | `test/unit/infrastructure/shell-adapter.test.ts` |

## What they pin that the ported unit tests do not

The ported assertions check the field each TS test checked (`data.command`,
`tokens.total`, …). These fixtures compare the **whole** event — every key of
`data`, its order, `tokens`, `errorKind` — which is what catches a renamed or
reordered key in a payload the orchestrator writes verbatim into
`runs/<id>.jsonl`.

`timestamp` is omitted: it comes from the injected clock, and the per-adapter
suites assert that separately.

## Regenerating

```sh
ORCH_BLESS_FIXTURES=1 cargo test -p orch-infra --test adapter_fixtures
```

Then **read the diff**. A change here is a change to the event wire contract
(PARITY.md §8.6), not a test detail.
