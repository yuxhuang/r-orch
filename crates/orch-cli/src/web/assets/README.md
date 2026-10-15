# Vendored front-end assets

Served by `orch web` from the binary (`include_str!`), so it works offline.
Both packages are MIT licensed; the licence texts sit alongside and are served
at `/licenses/`.

| File | Package | Version | Source | SHA-256 |
|---|---|---|---|---|
| `xterm.js` | `@xterm/xterm` | 5.5.0 | https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.js | `1f991ac3b4b283ebf96e60ae23a00a52765dd3a2e46fa6fdda9f1aab032f7495` |
| `xterm.css` | `@xterm/xterm` | 5.5.0 | https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.css | `ba8e6985669488981ccf40c0cefe3aba80722cb6c92de7ad628b0bd717faf2b6` |
| `LICENSE-xterm.txt` | `@xterm/xterm` | 5.5.0 | https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/LICENSE | `b569f629d00f2626a8100df2a1798210535621e42164dfd426a6fe5aac7b0ccd` |
| `addon-fit.js` | `@xterm/addon-fit` | 0.10.0 | https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/lib/addon-fit.js | `bdaefa370b1bfc42ee88d46fe6072400902a4d4b2d45cd93438dda9b23c97089` |
| `LICENSE-addon-fit.txt` | `@xterm/addon-fit` | 0.10.0 | https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/LICENSE | `e256f01188af527e4d06d21d06fbf785ae9c50d4b328bf03cbe0ba7f0aa4228f` |

`index.html` is ours, not vendored.

## Refresh

Set `XTERM` and `FIT` to the new versions (they must be a matching pair:
check the addon's `peerDependencies`), then from this directory:

```sh
XTERM=5.5.0 FIT=0.10.0 B=https://cdn.jsdelivr.net/npm
curl -fsSLO $B/@xterm/xterm@$XTERM/lib/xterm.js
curl -fsSLO $B/@xterm/xterm@$XTERM/css/xterm.css
curl -fsSL  $B/@xterm/xterm@$XTERM/LICENSE -o LICENSE-xterm.txt
curl -fsSLO $B/@xterm/addon-fit@$FIT/lib/addon-fit.js
curl -fsSL  $B/@xterm/addon-fit@$FIT/LICENSE -o LICENSE-addon-fit.txt
shasum -a 256 xterm.js xterm.css addon-fit.js LICENSE-*.txt
```

Then update the versions and hashes above and smoke-test the page in a browser.
