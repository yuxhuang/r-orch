# YAML output

status: active
owner: engineering lead
date: 2026-10-17
spec: none: this plan and ADR 0007 define the scoped contract

## Goal
Every command that can print JSON can print the same data as YAML, selected by a global `--yaml` flag, with `--json`
output unchanged.

## Non-goals
A new JSON schema, YAML or per-record streaming for `logs --follow` and `serve`, structured errors, a YAML log
encoding on disk, and `--yaml` for commands that never printed JSON.

## Context
`--json` is a global flag and every structured site printed through one of two JSON printers in `orch-cli`
(`output.rs`). `serde_yaml_ng` is the repository's YAML crate but cannot force the quoting the output needs
(ADR 0007).

## Design
`StructuredFormat::{Json, Yaml}` replaces the `json: bool` in `CliContext`; every call site that tested `ctx.json`
asks the format to print. `Cli` wraps clap's parser so `--json` together with `--yaml` is rejected wherever each is
placed. YAML is rendered from the same `serde_json::Value` as JSON by a small emitter that double-quotes every key
and string. Errors, `logs --follow` and `serve` are untouched. Rejected: serializing typed values with
`serde_yaml_ng` (tags, retyped scalars), a YAML-lines stream, an environment-variable switch.

## Tasks
| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| Y1 | Global `--yaml` output | encoding | `crates/orch-cli/src/{output,context,tree,lib,logs,doctor}.rs`, `crates/orch-cli/src/commands/*.rs` | — | `cargo test -p orch-cli --test yaml_contract` | 2 |
| Y2 | Quote every YAML scalar so all readers agree, keep errors as identical text under every flag, and emit long keys as explicit keys | encoding | `crates/orch-cli/src/output.rs` | Y1 | same | 2 |
| W1 | Docs | encoding | `README.md`, `CHANGELOG.md`, `docs/RUNBOOK.md`, `docs/ARCHITECTURE.md`, ADR 0007 | Y1, Y2 | README examples run in a disposable project; `--json`/`--yaml` parsed with an independent YAML reader | 2 |

## Status
| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| Y1, Y2 | implemented; contract suite in `yaml_contract.rs` |
| W1 | implemented in this PR; independent QA, final gate and publication pending |
| Known gap | `doctor` prints its human header before the structured document (inherited); not fixed here |

## Test plan
`yaml_contract.rs`: every command that accepts `--json` accepts `--yaml`, the two conflict wherever placed, and
for each structured command the YAML parses to the JSON value (including titles such as `null`, `on`, `1_000`,
`2024-01-15`). Binary-level error parity under `--json`/`--yaml`/neither. Run on macOS locally; Linux in CI.

## Rollback / compatibility
Additive: `--json` bytes, error text, exit codes and on-disk files are unchanged. Revert the `--yaml` flag and
`StructuredFormat::Yaml`.

## Docs to update
README ("Machine-readable output"), CHANGELOG (Added), RUNBOOK ("Structured output for `doctor`, `logs --follow`
and `serve`"), ARCHITECTURE (§3.3 module map, §4.2), ADR 0007.

## Open questions
- Should `doctor` keep its human header on stdout under `--json`/`--yaml`? Inherited; owner decision.
- Streams (`logs --follow`, `serve`) have no YAML form; a decision is needed before any is added.
