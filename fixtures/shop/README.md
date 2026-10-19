# Shop catalogue fixtures

`agent-shop.json`, `org-shop.json` and `default-agents.json` are hand-maintained
golden snapshots of r-orch's shop catalogue and default agents.
`crates/orch-domain/tests/shop_catalogue.rs` asserts the Rust catalogue — role
prompts included — matches them exactly.

When the catalogue or a role prompt in `crates/orch-domain/src/shop/` changes
intentionally, update the JSON here to match and review the diff.

The role prompts themselves live in `crates/orch-domain/src/shop/roles/*.md`
and are pulled in with `include_str!`. The files deliberately have **no
trailing newline**.
