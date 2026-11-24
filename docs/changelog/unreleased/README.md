One file per release note: `<slug>.<kind>.md`, one line. See AGENTS.md, "Changelog fragments", and the contract in `docs/specs/2026-11-23-changelog-fragments.md`. Keep this file when assembling releases.

- Size: the whole file, including its final newline, is at most 1,000 bytes (999 bytes of text plus the newline). A longer file is refused by `cargo xtask changelog check`.
- Interrupted release assembly: if `assemble` stops after writing `CHANGELOG.md`, consumed fragments can remain here and would be assembled again into the next release. Delete them, or restore `CHANGELOG.md` and this directory together and rerun. Steps: the spec section "Interrupted assembly".
