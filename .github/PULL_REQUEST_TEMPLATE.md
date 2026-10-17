## Summary

<!-- What changes, in a sentence or two. -->

## Why

<!-- Link the plan or ADR (docs/plans/..., docs/decisions/...) or write "no plan needed: <reason>". -->

## Changes

-

## Testing

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo xtask crate-graph`
- [ ] `cargo deny check` (if installed)
- [ ] `./xtask/gate.sh` (needed for layering, process-handling or security-relevant changes)

## Docs updated

- [ ] README
- [ ] RUNBOOK
- [ ] ARCHITECTURE
- [ ] CHANGELOG (Unreleased)
- [ ] none, because:

## Risk and rollback

<!-- One line: what could break, and how to undo it. -->
