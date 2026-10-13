# Golden prompt fixtures (V16 / V17)

Generated from the TypeScript engine, never by hand. `gen/generate.mts` imports
`template-engine.ts` and `skill-loader.ts` directly (`src/index.ts` does not
export them) and renders the full branch matrix of the default templates plus
custom-template edge cases.

```sh
cd parity/orch-ts
./node_modules/.bin/tsx /abs/path/to/fixtures/prompts/gen/generate.mts "$PWD" /abs/path/to/fixtures/prompts
```

Use absolute paths: `parity/orch-ts` is a symlink and tsx resolves relative
script paths against the link target.

- `<case>.json` — `{ name, template, context, expected }`. `context` is in the
  Rust `PromptContext` shape (explicit nulls, `shared_context` as `[k, v]`
  pairs); the generator converts it to the TS shape before rendering.
- `_templates.json` — the default system/user/legacy templates, checked against
  the `include_str!` copies in `crates/orch-infra/src/template/`.
- `_filter.json` — `filterRelevantContext` vectors (ordered output).
- `_skills.json` — skill blocks from the real library.

Checked by `crates/orch-infra/tests/{prompt_golden,skill_library}.rs`.
