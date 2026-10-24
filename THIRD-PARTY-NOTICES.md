# Third-party notices

r-orch is licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE)); [NOTICE](NOTICE) carries the
attribution notices. This file lists the third-party material included in this source tree, the files each
component covers, and the full text of each component's licence. Third-party components keep their own licences;
r-orch's Apache-2.0 licence does not replace them.

Rust crate dependencies are not in this tree: Cargo downloads them from crates.io with their own licence files,
and [`deny.toml`](deny.toml) limits which licences they may use. See [Rust crate dependencies](#rust-crate-dependencies).

| Component | Upstream | Licence | Files in this tree |
|---|---|---|---|
| ORCH | [oxgeneral/ORCH](https://github.com/oxgeneral/ORCH) | MIT | `crates/` (Rust source and ported tests, translated from ORCH's TypeScript), `skills/library/`, `crates/orch-domain/src/shop/`, `crates/orch-infra/src/template/*.liquid`, `fixtures/shop/`, `fixtures/prompts/`, `fixtures/adapters/` |
| gstack | [garrytan/gstack](https://github.com/garrytan/gstack) | MIT | `skills/library/*.md` (modified by ORCH), the skill text in `fixtures/prompts/_skills.json` |
| xterm.js (`@xterm/xterm` 5.5.0) | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | MIT | `crates/orch-cli/src/web/assets/xterm.js`, `xterm.css`, `LICENSE-xterm.txt` |
| `@xterm/addon-fit` 0.10.0 | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | MIT | `crates/orch-cli/src/web/assets/addon-fit.js`, `LICENSE-addon-fit.txt` |
| Catppuccin (colour values) | [catppuccin/palette](https://github.com/catppuccin/palette) | MIT | `crates/orch-tui/src/theme.rs` (`CATPPUCCIN_LATTE` and `CATPPUCCIN_MOCHA`) |
| Dracula (colour values) | [dracula/dracula-theme](https://github.com/dracula/dracula-theme) | MIT | `crates/orch-tui/src/theme.rs` (`DRACULA`) |
| Gruvbox (colour values) | [gruvbox-community/gruvbox](https://github.com/gruvbox-community/gruvbox) | MIT | `crates/orch-tui/src/theme.rs` (`GRUVBOX_DARK`) |
| Nord (colour values) | [nordtheme/nord](https://github.com/nordtheme/nord) | MIT | `crates/orch-tui/src/theme.rs` (`NORD`) |
| One Dark (colour values) | [joshdick/onedark.vim](https://github.com/joshdick/onedark.vim) | MIT | `crates/orch-tui/src/theme.rs` (`ONE_DARK`) |
| Solarized (colour values) | [altercation/solarized](https://github.com/altercation/solarized) | MIT | `crates/orch-tui/src/theme.rs` (`SOLARIZED_DARK` and `SOLARIZED_LIGHT`) |
| Tokyo Night (colour values) | [tokyo-night/tokyo-night-vscode-theme](https://github.com/tokyo-night/tokyo-night-vscode-theme) | MIT | `crates/orch-tui/src/theme.rs` (`TOKYO_NIGHT`) |

When you add material that originates outside this project, add a row and a section here in the same PR. Add a
line to `NOTICE` only when the material's licence requires one (for example, it comes with its own Apache-2.0
`NOTICE` file) or it substantially changes what r-orch is derived from; everything else belongs here.

## Distributing r-orch

This file covers the source tree. A copy distributed in any other form needs its notices carried with it:

- **A built `orch` binary, archive or package** compiles in ORCH-derived code, the default prompt templates and
  role prompts, the xterm.js files and the theme-derived colour palettes, and it statically links the Rust crate
  dependencies. Ship `LICENSE`, `NOTICE` and this file with it, together with the licence notices of the crates it links (see
  [Rust crate dependencies](#rust-crate-dependencies)). An install that copies `skills/library/` should copy
  `skills/library/LICENSE-MIT` with it.
- **Files that `orch init` writes** into `.orchestry/` (the default prompt template, and the Agent Creator role
  prompt in `agents/agt_creator.yml`) contain ORCH-derived text. If you redistribute them outside your project, include the ORCH MIT
  notice below.

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

## Catppuccin

- **Upstream:** https://github.com/catppuccin/palette, checked at commit
  `07d02aa110ef9eb7e7427afca5c73ba9cf7f8ebd`. Colours taken from `palette.json` (the `latte` and `mocha` flavours).
- **Licence:** MIT (the repository's `LICENSE` file).
- **Copyright:** Copyright (c) 2021 Catppuccin.
- **Files:** `CATPPUCCIN_LATTE` and `CATPPUCCIN_MOCHA` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
MIT License

Copyright (c) 2021 Catppuccin

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

## Dracula

- **Upstream:** https://github.com/dracula/dracula-theme, checked at commit
  `e22ebcc048b8aff586f9bc94db30ff5347070b42`. Colours taken from `README.md` (the colour palette table).
- **Licence:** MIT (the repository's `LICENSE` file).
- **Copyright:** Copyright (c) 2023 Dracula Theme.
- **Files:** `DRACULA` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
The MIT License (MIT)

Copyright (c) 2023 Dracula Theme

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

## Gruvbox

- **Upstream:** https://github.com/gruvbox-community/gruvbox, checked at commit
  `180ad85971343df68be3422a5630fa84e45a9ab2`. Colours taken from `colors/gruvbox.vim` (the palette definitions).
  gruvbox-community/gruvbox is the maintained fork of `morhetz/gruvbox` (whose README states MIT but which has no
  licence file); the palette values are the same.
- **Licence:** MIT (the repository's `LICENSE.md` file).
- **Copyright:** Copyright (c) 2018 Pavel Pertsev.
- **Files:** `GRUVBOX_DARK` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
MIT License

Copyright (c) 2018 Pavel Pertsev

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

## Nord

- **Upstream:** https://github.com/nordtheme/nord, checked at commit `1cef71605416a222e57225b544540ce0fcec18d4`.
  Colours taken from `src/nord.css` (`--nord0` to `--nord15`).
- **Licence:** MIT (the repository's `license` file).
- **Copyright:** Copyright (c) 2016-present Sven Greb.
- **Files:** `NORD` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
MIT License (MIT)

Copyright (c) 2016-present Sven Greb <development@svengreb.de> (https://www.svengreb.de)

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

## One Dark

- **Upstream:** https://github.com/joshdick/onedark.vim, checked at commit
  `47bec7a6196a843dad195d2666c3ac84c6e80c78`. Colours taken from `autoload/onedark.vim` (the `gui` colours).
  onedark.vim's colours are inspired by Atom's One Dark theme.
- **Licence:** MIT (the repository's `LICENSE` file).
- **Copyright:** Copyright (c) 2015 Joshua Dick.
- **Files:** `ONE_DARK` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
The MIT License (MIT)

Copyright (c) 2015 Joshua Dick

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

## Solarized

- **Upstream:** https://github.com/altercation/solarized, checked at commit
  `62f656a02f93c5190a8753159e34b385588d5ff3`. Colours taken from `README.md` (the colour values table).
- **Licence:** MIT (the repository's `LICENSE` file).
- **Copyright:** Copyright (c) 2011 Ethan Schoonover.
- **Files:** `SOLARIZED_DARK` and `SOLARIZED_LIGHT` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
Copyright (c) 2011 Ethan Schoonover

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

## Tokyo Night

- **Upstream:** https://github.com/tokyo-night/tokyo-night-vscode-theme, checked at commit
  `7c0f11eaef322f293621ca7befe462214b7ea468`. Colours taken from `README.md` (the colour palette table) and
  `themes/tokyo-night-color-theme.json` (`#3d59a1` and `#16161e`).
- **Licence:** MIT (the repository's `LICENSE.txt` file).
- **Copyright:** Copyright (c) 2018-present Enkia.
- **Files:** `TOKYO_NIGHT` in `crates/orch-tui/src/theme.rs`: the theme's documented hex colours, mapped onto
  r-orch's 26 palette tokens. The tinted backgrounds and any colour the theme lacks are r-orch's own derivations
  (listed as "Derived" in the comment above each table). No upstream file is copied.

```text
The MIT License (MIT)

Copyright (c) 2018-present Enkia

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## Rust crate dependencies

The crates in `Cargo.lock` are not part of this source tree, so this file does not reproduce their licences:
each crate's own licence files come with it from crates.io. A built binary statically links them, so a binary
distribution must ship the licence and copyright notices of the crates it actually links, generated from
`Cargo.lock` for that build (for example with `cargo about`). Include the supplementary notices that some crates
carry besides their top-level licence (third-party and Unicode-data notices inside the crate), and apply both
terms of every `AND` licence expression.

The following applies to any such distribution:

```text
Third-party components retain their own licences. The Apache-2.0 licence
for r-orch does not replace or restrict those licences.

option-ext 0.2.0 is governed by the Mozilla Public License, v. 2.0.
Its complete, unmodified Source Code Form is available from:
https://crates.io/api/v1/crates/option-ext/0.2.0/download
Upstream: https://github.com/soc/option-ext
Licence: https://mozilla.org/MPL/2.0/
Any modifications to its covered files distributed with future versions
must also be made available in Source Code Form under MPL-2.0, with the
applicable notices preserved. This obligation applies to the MPL-covered
files and does not change the licence of r-orch's separate source files.

For code with an MIT OR Apache-2.0 alternative, this distribution elects
Apache-2.0 unless a component-specific notice says otherwise.
Both terms of every AND expression continue to apply. Unicode data and
other embedded third-party material retain their accompanying notices.
```

`option-ext` is reached through `dirs` → `dirs-sys`; it is the one linked crate under a weak-copyleft licence.
r-orch does not modify it.
