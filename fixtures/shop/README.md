# Shop catalogue fixtures

`agent-shop.json`, `org-shop.json` and `default-agents.json` are generated
**from the TypeScript source**, not written by hand. They are the oracle for
V28 (`crates/orch-domain/tests/shop_catalogue_parity.rs`), which asserts the
Rust catalogue — role prompts included — matches them exactly.

Regenerate after bumping the pinned reference commit:

```sh
cd parity/orch-ts
./node_modules/.bin/tsx ../../fixtures/shop/generate.mjs "$PWD" ../../fixtures/shop
```

`extract-roles.mjs` writes the role prompts themselves into
`crates/orch-domain/src/shop/roles/*.md`, which the crate pulls in with
`include_str!`. The files deliberately have **no trailing newline**, matching
the TS template literals:

```sh
cd parity/orch-ts
./node_modules/.bin/tsx ../../fixtures/shop/extract-roles.mjs \
  ../../crates/orch-domain/src/shop/roles "$PWD"
```

Both are plain scripts rather than `xtask` subcommands because `xtask` is not
built yet; fold them in when it is.
