# 0007. A hand-written emitter for `--yaml` output

- Status: proposed
- Date: 2026-10-18

## Context

The global `--yaml` flag (see [the YAML output plan](../plans/2026-10-17-yaml-output.md)) prints the data `--json`
prints as YAML. The workspace already depends on `serde_yaml_ng` for parsing and for writing entity files, and
ARCHITECTURE §4.8 says to use a maintained crate for complex features. Two properties of the output are contract:
the YAML must parse to exactly the JSON value in every common reader, and `--json` bytes must not change.

## Decision

1. `orch-cli` renders `--yaml` from the `serde_json::Value` the JSON path would print (`output::StructuredFormat`),
   never from the typed value. An externally tagged enum would otherwise become a YAML `!Tag`, which parses back
   to something different from the JSON.
2. The Value is written by a small emitter in `output.rs` (about 115 lines, JSON values only): block style, two-space
   indent, every key and string written as a double-quoted scalar (JSON escaping, plus `\uXXXX` for `U+007F`-`U+009F`,
   `U+2028`, `U+2029`, `U+FEFF`, `U+FFFE`, `U+FFFF`), floats with an exponent written with a decimal point and signed
   exponent, keys over 128 escaped bytes written as explicit `? key` entries, no tags, no anchors, no document marker.
3. This is the §4.8 "no maintained crate fits" exemption, claimed in the code comment for the crates the workspace
   already depends on; no survey of other YAML emitter crates is recorded here. The comment says that `serde_yaml_ng` cannot force
   every string and key to be double-quoted, which leaves YAML 1.1 readers free to retype `on`, `1_000`, `<<` or a
   date. The parse-back tests in `crates/orch-cli/tests/yaml_contract.rs` cover those titles.
4. Dependency impact: none new. `serde_yaml_ng` was already a workspace dependency; `orch-cli` now declares it too,
   and only its output tests use it.

## Consequences

- YAML and JSON output cannot drift: both come from one Value, in one key order.
- The output is verbose (everything quoted) and has one style; there is no `--yaml` pretty-printing option.
- The emitter is ours to maintain. It covers only the shapes `serde_json::Value` can hold, and the tests round-trip
  it through `serde_yaml_ng`. Any new Value shape needs a test.
- Streams are not covered: `logs --follow` and `serve` do not honour `--yaml`; a per-record `---` stream would need
  its own decision.
- Rollback: remove `StructuredFormat::Yaml`, the `--yaml` flag and the emitter; `--json` is untouched.
