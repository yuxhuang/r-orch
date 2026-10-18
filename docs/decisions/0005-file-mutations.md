# 0005. File-based update and patch: RFC 7396 via `json-patch`

- Status: accepted
- Date: 2026-10-18

## Context

`update --file` and `patch --file` (planned in [the file-mutations plan](../plans/2026-10-17-file-mutations.md))
need an RFC 7396 JSON Merge Patch over `serde_json::Value`: objects merge recursively, `null` removes a member and
arrays replace. The domain's `deep_merge` implements defaults layering, not removal, so it cannot be reused.
Merge Patch is a complex-enough contract, with well-known corner cases, to fall under the library-first policy
(ARCHITECTURE §4.8). Primary sources checked 2026-10-18: [RFC 7396 §2](https://www.rfc-editor.org/rfc/rfc7396.html),
[`json_patch::merge`](https://docs.rs/json-patch/4.2.0/json_patch/fn.merge.html), the crates.io index and the crate sources
in the registry cache.

## Decision

1. Adopt `json-patch` 4.2.x with `default-features = false`, declared in `[workspace.dependencies]` and consumed
   only by `orch-cli` (where `document_input::apply_mutation` lives). The default `diff` feature (an RFC 6902
   patch generator) is off; only `merge` is used. No other crate depends on it.
2. It brings one new transitive crate, `jsonptr` 0.7.1. `Cargo.lock` gains exactly those two packages; no
   existing locked version moves.
3. Reject a hand-written merge (no exemption in §4.8 applies: a maintained crate fits) and RFC 6902 (more
   authority and surface than the feature needs).

## Checks (ARCHITECTURE §4.8)

| Check | `json-patch` 4.2.0 | `jsonptr` 0.7.1 |
|---|---|---|
| Licence | MIT/Apache-2.0 (manifest, LICENSE-MIT and LICENSE-APACHE shipped); permitted by `deny.toml` | MIT OR Apache-2.0 (same) |
| MSRV | none declared (edition 2021); builds on the pinned 1.99.1 (`cargo check --locked --workspace`) | 1.79.0 |
| Maintenance | released 2026-04-30; prior releases 2025-09 and 2025-02; not archived; about 118M downloads | released 2025-02-16; a newer 0.8 line exists but `json-patch` 4.2 requires 0.7 |
| Yanked | 4.2.0 is not yanked | 0.7.1 is not yanked (0.7.0 is) |

## Consequences

- `merge` recurses once per nesting level and does not bound depth or size. Callers keep the existing file-size
  and depth limits and validate the target before an atomic write; the dependency is not a limit.
- `merge` creates an object for a non-object target and removes a member whose patch value is `null`, including
  under a newly created object (`{"new": {"x": null}}` yields `"new": {}`). Contract tests in the U1 slice pin this.
- When `json-patch` moves to `jsonptr` 0.8, a second `jsonptr` could enter the graph only through another
  dependent; none exists today.
- Rollback: remove the three manifest lines and `json-patch`/`jsonptr` from `Cargo.lock` with the U1 callers.
