# 0006 — Versioned directory org templates and portable rule expansion

## Status
Accepted, 2026-10-17, after the design challenge and lead adjudication. The YAML-output sentence of Decision ¶3 is superseded by [0008](0008-yaml-output-has-no-stream-form.md); the rest stands.

## Context
Constant org and shop catalogues cannot express a user-managed formation with separate role files. Runtime Agent records contain execution state and are unsuitable as portable templates. Shared rule paths can be missing in a deployed worktree. Existing presets and runtime storage must remain compatible.

## Decision
Define schema_version 1 bundles: org.yml, agents/<safe-name>.yml, rules/<safe-id>.md. Org metadata owns the lead, defaults and available shared rules. Agent files own instructions, optional adapter/model/configuration, skills and rule selection. Optional inheritable fields retain presence: absent inherits, present empty overrides, patch-null removes, and definition/full-update null rejects. Authoring outputs never materialize inherited defaults. Validate complete bundles and bounded JSON-compatible YAML before use, rejecting unknown schema keys, unsafe paths, mismatched names, missing lead/rules and symlinks. Builtins are embedded source files; named user templates take precedence unless builtin: is explicit. Directory selection is exact. Legacy constants remain supported.

Recognize canonical .claude/agent-rules/<id>.md references, normalize the extracted shorthand headers with recorded diffs, and reject unresolved rule basenames. Resolve explicit and textual transitive selections once in registry order into embedded sections, replacing rule-file references without removing instruction content. Do not copy rule files into the execution workspace. Deployed entities are snapshots and never silently synchronized. One file per named member replaces replicas. Template mutations validate the whole candidate before atomic publication. Full replacement reads/bounds a target but overlays its bytes before parsing; deletion omits the target and validates the remainder. Patch/editor need a parseable base. Filename/replacement identity, root-repair selector identity, lead protection, unrelated-file validation and containment remain enforced. Deploy preflights all inputs, then uses ordinary services and explicitly reports survivors on partial failure.

ADR 0005 (file mutations) specifies json-patch adoption and writable-record boundaries. YAML output uses the same serde value as JSON and YAML document framing for streams; persisted JSONL is unchanged.

> Superseded by [ADR 0008](0008-yaml-output-has-no-stream-form.md): streams (`logs --follow`, `serve`) have no YAML form, and there is no per-record YAML framing. The rest of this paragraph stands.

## Consequences
Templates are inspectable, versionable, portable and bounded. Existing preset/runtime formats remain usable without migration. Rule text is duplicated in deployed roles, increasing size within the existing record budget. Runtime deployment remains nontransactional and existing entity writers remain last-writer-wins; neither is disguised as a database transaction. Builtins require an explicit init-copy before editing. Directory schema evolution requires a new supported version, not silently ignored fields.
