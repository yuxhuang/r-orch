# File definition updates and merge patches

status: active
owner: engineering lead / senior engineer
date: 2026-10-17
spec: none: this plan records the accepted file-mutation scope; the companion dependency ADR records parser and merge choices

## Goal
Add non-editor `update` and `patch` commands for the existing agent, task and project-config editor surfaces. Accept a full or partial JSON/YAML document from a file or stdin, validate it before persistence and preserve service-owned runtime state. Share the input API with subsequent organization-template mutations.

## Non-goals
Global preference mutations, entity lifecycle or assignment replacement, remote record mutation, runtime writer transaction redesign, template implementation in this PR, new dependencies before the exclusive dependency-preparation step, or changes to existing editor commands and JSON output contracts.

## Context
Agent/task services own entity state; project config retains unknown extension keys. Existing guarded YAML parsing and atomic writes in `orch-infra` provide reusable boundaries. Organization-template work needs separate command ownership so it can proceed alongside live-definition mutations. See [ARCHITECTURE §4.8](../ARCHITECTURE.md#48-library-first-policy-user-directive-2026-10-13) and the companion organization-template plan when it lands.

## Design
Prepare mechanical seams before parallel work. Move org clap types and nested dispatch into `commands/org.rs`, reexport the types from `tree.rs`, and delegate shop through `template_shop::ShopArgs`. Pre-parse routing delegates nested org initialization policy; existing org operations remain guarded. Register module seams without advertising unfinished commands. Expose `storage::fs_utils::parse_yaml_bytes` using the existing size, alias and expanded-node controls; keep existing file reads on this path. The shared CLI input module later adds strict JSON-compatible object validation for mutation documents.

Every mutation requires `-f/--file FILE_OR_DASH`; `--input-format json|yaml` overrides detection. Otherwise use JSON for `.json`, YAML for `.yml`/`.yaml`, and sniff unknown extensions/stdin from the first nonwhitespace byte (`{`/`[` selects JSON). A selected parser failure is final. Require exactly one nonempty object document; reject duplicate/non-string keys, tags, nonfinite values, depth over 64 and documents over 1 MiB, including stdin/editor buffers. Output format is independent of input format.

`update` replaces the writable projection: omitted optional fields clear, omitted required fields fail. `patch` applies RFC 7396 using `json-patch` 4.2.x with default features disabled: null removes a member, arrays replace, objects merge. Reject root nonobjects and unknown/protected paths before merging, including null-valued unknown keys. Existing `deep_merge` implements defaults and is not the patch operation. Dependency preparation owns manifests, lockfile, toolchain/license verification and its ADR; this seam preparation does not change them.

Writable projections:

| Target | Replacement | Preserved / excluded |
|---|---|---|
| Agent | Required name, adapter, config; optional role; strict current AgentConfig fields | Identity, runtime/status/statistics/current-task fields, capabilities and remote metadata |
| Task | Required nonblank title, integer priority 1–4 and string description; labels default empty | Identity, lifecycle, assignment, dependencies, timestamps, attempts, workspace, goal, attachments, scope and review/proof state |
| Project config | Raw project configuration; full update requires project/defaults/scheduling and their required typed fields | Global preferences are outside this scope; unknown extension keys survive |

For full entity-show input, accept only known read-only echoes equal to current values; reject differing values. Patches always reject read-only entries. Agent/task mutations reject running and remote-controlled records. Re-read immediately before a single service save and preserve read-only state. Shell/name/duplicate validation stays in the agent service. Config validation builds the effective default-merged typed view but persists the raw candidate, preserving extensions. Patch-null on a default-backed config key restores the default; `{}` makes no write.

Parse, merge and validate before atomic publication. New mutation operations serialize with bounded locks; old edit/runtime writers retain their existing last-writer behavior. Entity/index persistence is not one transaction. Errors name source/field without dumping role/env content; machine success is emitted after persistence. A crash after rename may commit before stdout, so inspect current state before retrying. Template replacement/deletion candidate overlays and template locks belong to the later template store, which consumes the shared input API.

Rejected: bespoke recursive patching, RFC 6902 operations, overwriting a deserialized live entity, typed config round trips that discard extensions, and a new general service framework.

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| H0 | Prepare command ownership and guarded parser seams | Disjoint ownership | CLI central wiring/org/shop, module registries, infra fs_utils, this plan | Reviewed YAML output and shop display | Existing CLI/JSON/YAML/shop tests; parser guards; fmt/clippy/crate graph | 3 |
| H0D | Pin mutation dependencies and document adoption | Library/toolchain/license boundary | Manifests, lockfile, dependency ADR | Verified H0 commit | Locked build, dependency graph and licenses | 3 |
| U1 | Implement live-definition update/patch and shared input | Accepted writable projections | CLI document_input/tree/lib/routing and agent/task/config commands; agent/task services; raw config storage | H0D | Hermetic update/patch integration tests | 3 |
| UTT | Test file-mutation contracts independently | Error and preservation cases | CLI update_patch integration tests | Real U1 SHA and public input API | Fail-before/pass-after checks; full relevant suite | 3 |
| P2 | Consume shared input for template update/patch | Input API reuse | Nested org/template commands and distinct template tests | U1 plus verified template implementation/tests | Template candidate repair, inheritance/null and immutability tests | 4 |
| W1 | Document verified behavior | User/operator documentation | README, CHANGELOG, RUNBOOK, ARCHITECTURE | Completed implementations | Disposable-project command examples and fact check | 3/4 |

## Status

| Item | State (implemented / verified on platform / pending) |
|---|---|
| H0 | Mechanical seam implementation; exact verification and commit recorded in the engineering handoff |
| H0D | Dependency pinned and verified on macOS; accepted adoption ADR 0005 |
| U1 | Live definitions and shared input implemented; macOS verification recorded in the engineering handoff; independent UTT/reviews pending |
| UTT / P2 / W1 | Pending; no completion is claimed by this plan |

## Test plan
Use disposable projects and homes for test executables, isolated global config, shell/stub adapters and no real AI CLIs. Build tools retain their real HOME with an isolated CARGO_TARGET_DIR. Verify JSON/YAML input and stdin, parser budgets, malformed/duplicate/tagged/multiple documents, exact immutable fields, unknown-null rejection, optional clearing, null removal, array replacement, empty patches, raw extension preservation, running/remote guards, lock timeouts and write failures. Check existing JSON bytes, YAML equivalence, routing guards and legacy shop behavior. New behavior tests must fail when the change is reverted.

Run `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`, `cargo xtask crate-graph` and `cargo deny check` when installed. Follow AGENTS.md cold-gate requirements for security-relevant changes; record unavailable oracle/tooling and platforms explicitly. Final code/security reviews use a frozen exact SHA; downstream builds must not treat blocked handoffs as completed work.

## Rollback / compatibility
No runtime schema migration. Existing presets, JSON schemas and editor commands remain compatible. Reverting the new command implementation leaves stored entities readable. Back up writable definitions before replacement. Existing runtime writers remain last-writer-wins; stop the runner and use a single writer for reproducible bulk maintenance. Reverting does not restore an already replaced definition.

## Docs to update
The dependency owner writes the update/patch adoption ADR before implementation. The technical writer owns README command/input examples, CHANGELOG additions, RUNBOOK input/locking/retry failures and ARCHITECTURE ownership/concurrency. The template plan and ADR cover bundle schemas and candidate validation separately.

## Open questions
- None in the accepted scope. Dependency compatibility and final implementation reviews remain verification gates.
