# Golden prompt fixtures

Hand-maintained golden snapshots of r-orch's prompt rendering: the full branch
matrix of the default templates plus custom-template edge cases. When the
templates or renderer change intentionally, update the JSON here to match and
review the diff.

- `<case>.json` — `{ name, template, context, expected }`. `context` is in the
  Rust `PromptContext` shape (explicit nulls, `shared_context` as `[k, v]`
  pairs).
- `_templates.json` — the default system/user/legacy templates, checked against
  the `include_str!` copies in `crates/orch-infra/src/template/`.
- `_filter.json` — relevant-context filter vectors (ordered output).
- `_skills.json` — skill blocks from the real library.

Checked by `crates/orch-infra/tests/{prompt_golden,skill_library}.rs`.
