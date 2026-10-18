# 0008. `--yaml` output has no stream form

- Status: accepted
- Date: 2026-10-18
- Supersedes: the YAML-output sentence of [ADR 0006](0006-org-templates.md) Decision ¶3

## Context

ADR 0006 Decision ¶3 says: "YAML output uses the same serde value as JSON and YAML document framing for streams".
No such framing was built. [ADR 0007](0007-hand-written-yaml-output-emitter.md) and
[the YAML output plan](../plans/2026-10-17-yaml-output.md) leave streams out of `--yaml`, and the README and
RUNBOOK ("Structured output for `doctor`, `logs --follow` and `serve`") document that. The board fact check of the
template stack found that the accepted ADR states a contract that does not exist. ADR 0006 is accepted, so the
sentence is superseded here rather than rewritten.

## Decision

1. `--yaml` renders the `serde_json::Value` that `--json` would print, as one YAML document, through the emitter of
   ADR 0007. This replaces the "same serde value" wording of ADR 0006.
2. Streams have no YAML form. `logs --follow` prints a text banner and then live events, and `serve` writes JSON
   Lines (`--log-format json`, the default) or text, under every output flag. There is no per-record YAML
   framing and no `---` document stream.
3. Persisted `.jsonl` logs are unchanged, as ADR 0006 already says.
4. The rest of ADR 0006 stands.

## Consequences

- The ADRs, the plans, the README and the RUNBOOK describe one stream contract.
- A YAML stream would need its own decision, as ADR 0007 says.
