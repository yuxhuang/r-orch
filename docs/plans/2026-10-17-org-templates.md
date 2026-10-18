# Organization templates and a-team

status: active
owner: engineering lead
date: 2026-10-17
spec: none: this plan and the org-templates ADR define the scoped contract

## Goal
Deploy and manage versioned directory templates with one YAML per agent, including an embedded a-team. Load template agents in shop, support YAML wherever JSON is supported, and replace/patch editor-managed definitions from files or stdin.

## Non-goals
Remote registries, automatic skill installation, template inheritance/replicas, automatic updates to deployed agents, migrating legacy presets, or a runtime storage transaction redesign.

## Context
The existing org catalogue is constant data in orch-domain; CLI deploy uses agent/team/goal services. Editor commands and structured output already exist. Guarded YAML parsing and atomic storage writes are reusable. See the org-templates ADR and ARCHITECTURE §4.8.

## Design
Use schema_version: 1 in org.yml, agents/<name>.yml and rules/<id>.md. Strict safe identifiers and containment precede mutations/deploy; complete candidate validation precedes publication. Full replacement overlays a bounded corrupt target before parsing and deletion validates the remainder; patch/edit need a parseable target. Authoring fields preserve omission versus explicit empty through show/update; null patches remove overrides while null definitions reject. Normalize the extracted rule headers to canonical references, reject ambiguous shorthand, resolve the complete rule closure once and embed it in deployed roles. Resolve explicit directory, otherwise user then builtin/legacy, with source-qualified selectors. Keep old presets intact. Use json-patch for RFC 7396 mutations, writable projections for live records, and serde_yaml_ng for YAML of existing JSON values. Single-file writes are atomic; deployment can leave explicitly reported partial results. Reject replicas and runtime upsert as unnecessary authority/complexity.

## Tasks
| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| S1 | Repair measured shop display failures | display | shop_picker.rs and measured TUI paths | T0 reproduction | PTY regressions | 1 |
| Y1 | Global YAML output | encoding | orch-cli output/context/call sites | — | JSON/YAML contract suite | 2 |
| H0 | Prepare command seams and dependency | ownership | central CLI wiring, guarded parser, manifests/lock | Y1; S1 before picker handover | old CLI tests, crate graph | 3 |
| U1 | Agent/task/config update and patch | mutation | document_input, target commands/services | H0 | update_patch integration suite | 3 |
| P1 | Template types, assets, CRUD, deploy, shop | formation | org_templates modules, org commands, template_shop | H0, verified role extraction, S1 | org_templates suite | 4 |
| P2 | Template file update/patch | reuse | org_template commands/tests | U1, P1 | null/immutable/error tests | 4 |
| W1 | User/operator/architecture docs | docs | README, CHANGELOG, RUNBOOK, ARCHITECTURE | final behavior | examples with temporary project | each matching PR |

## Status
| Item | State (implemented / verified on platform / pending) |
|---|---|
| S1 / Y1 / H0 | implemented and verified in prerequisites |
| P1 | implemented; verification recorded in the implementation handoff |
| U1 | integrated from the reviewed C1 lineage; independent integration gates remain |
| P2 | implemented using document_input and the locked raw candidate overlay; focused mutation tests added; integration verification and independent review remain |
| W1 | later documentation pipeline; not certified by P2 |

## Test plan
Hermetic CLI integration tests cover names/path traversal/symlinks, duplicate/unknown keys, schema/lead/rules, source precedence, all CRUD and deploy partial failure, RFC 7396 null/array behavior, immutable fields, atomic failures, and JSON/YAML equivalence/conflicts/streams. Verify complete sanitized role content and rule expansion independently, including red-team/team-lead shorthand normalization, three-rule closure, omitted/empty/nonempty skills round trips with shop/deploy parity, and full replacement/deletion of malformed targets with unrelated corruption rejected. PTY tests cover the measured shop defect, selection/cancel, resize and terminal restoration. Run on macOS locally and Linux CI; report any unavailable platform explicitly. Run fmt --all --check, clippy --workspace --all-targets -- -D warnings, test --workspace, xtask crate-graph and cargo deny check when installed. Follow AGENTS.md cold-gate requirements for security-relevant changes and document missing oracle/tooling rather than claiming an unrun gate.

## Rollback / compatibility
No stored runtime entity migration. Existing constants and JSON payloads remain compatible; YAML is additive. Directory templates are authoring files; older binaries ignore them. Deployed rules are ordinary self-contained role strings. Reverting code does not delete agents. Failed deployment names surviving entities for explicit cleanup.

## Docs to update
README commands/examples, CHANGELOG additions/fixes, RUNBOOK validation/partial-deploy/concurrency errors, ARCHITECTURE ownership/data flow, org-template ADR and update-patch dependency ADR.

## Open questions
- Accepted contract: no functional open questions. Independent testing/reviews and integration remain separate gates.
