# Third-party notices

r-orch is licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE)); [NOTICE](NOTICE) carries the
attribution notices. This file lists the third-party material included in this source tree, the files each
component covers, and the full text of each component's licence.

Rust crate dependencies are not in this tree: Cargo downloads them from crates.io with their own licence files,
and [`deny.toml`](deny.toml) limits which licences they may use.

| Component | Upstream | Licence | Files in this tree |
|---|---|---|---|
| ORCH | [oxgeneral/ORCH](https://github.com/oxgeneral/ORCH) | MIT | `crates/` (Rust source and ported tests, translated from ORCH's TypeScript), `skills/library/`, `crates/orch-domain/src/shop/`, `crates/orch-infra/src/template/*.liquid`, `fixtures/shop/`, `fixtures/prompts/`, `fixtures/adapters/` |
| gstack | [garrytan/gstack](https://github.com/garrytan/gstack) | MIT | `skills/library/*.md` (modified by ORCH), the skill text in `fixtures/prompts/_skills.json` |
| xterm.js (`@xterm/xterm` 5.5.0) | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | MIT | `crates/orch-cli/src/web/assets/xterm.js`, `xterm.css`, `LICENSE-xterm.txt` |
| `@xterm/addon-fit` 0.10.0 | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | MIT | `crates/orch-cli/src/web/assets/addon-fit.js`, `LICENSE-addon-fit.txt` |

When you add material that originates outside this project, add a row and a section here (and a line in
`NOTICE`) in the same PR.

## ORCH

- **Upstream:** https://github.com/oxgeneral/ORCH, the TypeScript orchestrator that r-orch began as a Rust port
  of. Checked at commit `c066dc013e98e08d753ce204e604e04621dd4167`.
- **Licence:** MIT (the repository's `LICENSE` file; `package.json` declares `"license": "MIT"`).
- **Copyright:** Copyright (c) 2026 Agents Organizations Contributors.
- **Files:** r-orch's Rust source began as a translation of ORCH's TypeScript source; those portions and the
  files listed below are derived from ORCH and are used under the MIT License below.
  - `crates/`: the Rust source, including the tests ported from ORCH's `test/` suites. The module structure,
    domain model, state machine (for example `crates/orch-domain/src/transitions.rs`, from ORCH's
    `src/domain/transitions.ts`), config schema and CLI and TUI text began as a translation of ORCH's `src/`;
    most of today's code is r-orch's own work built on that core.
  - `skills/library/*.md`: the 26 library skills, copies of ORCH's `skills/library/` (byte-identical at the
    commit above). ORCH adapted them from gstack's skills, so they are also covered by the [gstack](#gstack)
    notice. Both licences are also in [`skills/library/LICENSE-MIT`](skills/library/LICENSE-MIT), so they travel
    with copies of that directory.
  - `crates/orch-domain/src/shop/`: the agent-shop and organization-shop catalogues and the role prompts in
    `roles/*.md`, copied from ORCH's `src/domain/agent-shop.ts`, `src/domain/org-shop.ts` and
    `src/domain/default-agents.ts`.
  - `crates/orch-infra/src/template/system.liquid` and `user.liquid`: the default prompt templates, derived from
    ORCH's `src/infrastructure/template/template-engine.ts`.
  - `fixtures/shop/` and `fixtures/prompts/`: golden snapshots of the catalogue and the prompt templates above.
  - `fixtures/adapters/`: recorded adapter output, originally taken from ORCH's test suites (the source of each
    fixture is listed in [`fixtures/adapters/README.md`](fixtures/adapters/README.md)).
  - Any other part of r-orch derived from ORCH.

```text
MIT License

Copyright (c) 2026 Agents Organizations Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## gstack

- **Upstream:** https://github.com/garrytan/gstack, Garry Tan's collection of agent skills. Compared at commit
  `407b15692073dfd8aaff13e755d99a6022ff8ae5`, the last gstack commit before ORCH added its skill library.
- **Licence:** MIT (the repository's `LICENSE` file).
- **Copyright:** Copyright (c) 2026 Garry Tan.
- **Files:**
  - `skills/library/*.md`: ORCH's adaptations of gstack's `<skill>/SKILL.md` files (`upgrade.md` comes from
    `gstack-upgrade/SKILL.md`). ORCH modified them, removing gstack's preamble and telemetry blocks; r-orch ships
    ORCH's modified copies unchanged. ORCH's changelog describes them as "adapted from gstack".
  - `fixtures/prompts/_skills.json`: golden snapshots that embed the text of the skills above.

```text
MIT License

Copyright (c) 2026 Garry Tan

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## xterm.js

- **Upstream:** https://github.com/xtermjs/xterm.js, npm package `@xterm/xterm` 5.5.0.
- **Licence:** MIT (the package's `LICENSE` file and npm metadata).
- **Files:** `crates/orch-cli/src/web/assets/xterm.js`, `xterm.css` and `LICENSE-xterm.txt`, unmodified (source
  URLs and SHA-256 hashes in [`crates/orch-cli/src/web/assets/README.md`](crates/orch-cli/src/web/assets/README.md)).
  They are compiled into the `orch` binary, and `orch web` serves the licence at `/licenses/xterm.txt`.
- `xterm.css` keeps its own header, which also credits term.js (Copyright (c) 2012-2013, Christopher Jeffrey,
  MIT License) and notes that the terminal was originally forked, with the author's permission, from Fabrice
  Bellard's JavaScript vt100 for jslinux (Copyright (c) 2011 Fabrice Bellard).

```text
Copyright (c) 2017-2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)
Copyright (c) 2014-2016, SourceLair Private Company (https://www.sourcelair.com)
Copyright (c) 2012-2013, Christopher Jeffrey (https://github.com/chjj/)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## @xterm/addon-fit

- **Upstream:** https://github.com/xtermjs/xterm.js (`addons/addon-fit`), npm package `@xterm/addon-fit` 0.10.0.
- **Licence:** MIT (the package's `LICENSE` file and npm metadata).
- **Files:** `crates/orch-cli/src/web/assets/addon-fit.js` and `LICENSE-addon-fit.txt`, unmodified. They are
  compiled into the `orch` binary, and `orch web` serves the licence at `/licenses/addon-fit.txt`.

```text
Copyright (c) 2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```
