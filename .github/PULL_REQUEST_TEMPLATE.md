## Summary

<!-- What changes, in a sentence or two. -->

## Why

<!-- Link the spec, plan or ADR (docs/specs/..., docs/plans/..., docs/decisions/...) or write "no plan needed: <reason>".
     For spec-driven work, name the task (T#) and the requirements it covers (R#). -->

## Changes

-

## Testing

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo xtask crate-graph`
- [ ] `cargo deny check` (if installed)
- [ ] `./xtask/gate.sh` (needed for layering, process-handling or security-relevant changes)

Ran on: <!-- macOS / Linux / CI only. Name any check that is env-limited and failures that also occur on the base commit. -->

## Docs updated

- [ ] README
- [ ] RUNBOOK
- [ ] ARCHITECTURE
- [ ] CHANGELOG (Unreleased)
- [ ] Spec / plan (status line; the spec Verification table if this completes the work)
- [ ] none, because:

## Risk and rollback

<!-- One line: what could break, and how to undo it. -->
