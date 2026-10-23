# 0010. Prompt mode and the adapter effort contract

- Status: proposed
- Date: 2026-10-22

## Context

The owner asked for an opt-in token-saving mode that leaves the current behaviour unchanged, and for every
adapter's effort settings to match its current CLI ([spec](../specs/2026-10-22-token-eco-mode.md),
[plan](../plans/2026-10-22-token-eco-mode.md)). Both change on-disk and CLI contracts, and the effort work
changes two frozen `orch-core` surfaces ([TRAITS-FROZEN.md](../TRAITS-FROZEN.md), Amendment 2).

Facts that forced the shape:
- In the frozen 28-agent baseline, other agents' full roles are 93.77% of the rendered prompt bytes.
- `opencode` v1 and v2 ship under the same executable name with different variant syntax, so a `#<level>`
  suffix that v2 accepts can break a v1 install in unattended dispatch.
- No provider exposes the reasoning level it actually used to the adapters orch drives.
- v0.9.4 accepted `low|medium|high` for every adapter, including ones that ignore it.

## Decision

1. Add `defaults.agent.prompt_mode: normal|eco` (default `normal`, written by `orch init`) and an optional
   per-agent `config.prompt_mode` override that is never copied from the project value. Add the optional
   `config.role_summary` (at most 192 UTF-8 bytes). The `--json` shape of `agent show`/`list` does not change;
   `agent show --effective` adds an envelope.
2. Keep normal prompt rendering byte-identical by rendering eco with separate templates and a separate
   projection, selected by `PromptContext.eco`.
3. Refuse eco together with a custom prompt template, at write time and again at dispatch. Do not translate
   templates.
4. Validate `config.effort` against one dated, per-adapter capability table in `orch-domain` whenever a
   definition is written. At dispatch, treat `low|medium|high` as legacy: forward where the adapter can,
   otherwise ignore it with a warning, never fail. Validate other values strictly at dispatch too.
5. Add `Adapter::prepare_effort`, an async pre-spawn hook with an identity default. Only OpenCode overrides it,
   to run a bounded `--version`/`run --help` check before synthesizing a variant suffix. `execute` stays
   synchronous and still returns the model child's PID.
6. Describe every forwarded effort as a request; never report an effective level.
7. Do not reuse CLI sessions in either mode.

## Consequences

- Eco can be turned on and off per project or agent from the next dispatch, with no data migration.
- Codex and Antigravity agents with a stored effort now pass it to the CLI; a Codex agent with effort and no
  model may get a different model than its managed default. Both are warned about.
- Definitions with the new effort values are not readable by v0.9.4; downgrade requires clearing them first.
- An OpenCode dispatch with an explicit effort spawns up to two short probe processes first.
- Every adapter implementation and test double compiles unchanged (default method). A new production caller
  of `execute` must await `prepare_effort` first.
- The capability table must be refreshed by hand when a CLI changes; there is no discovery service.
