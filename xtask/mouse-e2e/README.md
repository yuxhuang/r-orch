# TUI mouse end-to-end harness

Proves mouse input reaches `orch tui` and changes what is drawn, in three real environments.
It drives the real binary and asserts on the rendered screen text.

```
xtask/mouse-e2e/run.sh [--no-build] [--only tmux-sgr,tmux-fwd,xterm-web]
```

Builds `orch-cli` (`cargo build -p orch-cli`, HOME untouched), runs everything, prints one
`PASS|FAIL|INFO <env>/<check> [control|mouse] detail` line per check, exits non-zero on any FAIL.
`KEEP=1` keeps the throwaway dirs. `E2E_ROOT` sets where they go (default: system temp dir).
`ORCH_BIN`, `PW_PYTHON`, `PLAYWRIGHT_BROWSERS_PATH` override the binary and the Playwright install.

## Environments

| env | how input arrives | proves |
|---|---|---|
| `tmux-sgr` | `tmux send-keys -l` writes raw SGR (`ESC[<0;X;YM` / `m`, wheel `64`/`65`) into the pane | the app's own mouse parsing, independent of tmux mouse mode |
| `tmux-fwd` | tmux (`mouse on`) with a real client attached in a Python PTY; SGR bytes written to the client as an outer terminal would | tmux translates and forwards to the app, which must have enabled mouse reporting |
| `tmux-fwd-off` | same with `mouse off` | the `mouse` checks are `INFO` only (no pass/fail); its four keyboard `control` checks are still real gates |
| `xterm-web` | `orch web` + headless Chromium (Playwright); real `page.mouse` click/wheel on xterm.js at cell pixel coordinates | the browser path end to end |

Screen reading: tmux `capture-pane -p` (a real terminal emulator); xterm.js `term.buffer.active`.
Click coordinates are found by searching the captured text for the label. The two drag checks are
the exception: they drag a fixed `(4,6)→(30,6)` span, because what they assert is the *selection*,
not which cells it covers.

## Checks (each env)

`control` checks use the keyboard and must pass today; they prove the harness reads the screen.
`mouse` checks are the feature under test.

- `tab-<agents|goals>.keyboard` / `.click`: click the header label; the upper-cased label (active tab) changes.
- `row-select.keyboard` / `.click`: click the unselected task row; the `▸` marker moves there, and Enter opens `DETAIL ━ <title>` for it.
- `wheel.keyboard` / `.scroll`: wheel-down over the list moves the selection down, wheel-up moves it back. Where tmux/xterm.js mediate, the check additionally FAILs if the app never enabled mouse reporting (both turn wheel into Up/Down keys on the alternate screen, which would be a false green).
- `wheel.cap`: `WHEEL_BURST` (10) notches delivered in **one write** move the selection by exactly
  `WHEEL_CAP` (3) — the per-frame cap. This is the check that cannot false-green on alternate
  scroll, which gives one row per notch. A browser cannot deliver a burst atomically, so
  `xterm-web` reports it as `INFO`; there, `app-enables-mouse-reporting` rules out the fallback by
  reading xterm.js's own tracking mode.
- `selection.shift-drag` (`xterm-web`): while the app holds mouse reporting, a plain drag selects
  nothing and **Shift+drag still selects text**. The design stated this at ~85 % confidence and
  gated it here; it passes.
- `selection.option-drag` (`xterm-web`): the macOS half. xterm.js routes force-selection to
  `altKey` only when `isMac`, and to `shiftKey` otherwise, so off macOS this is `INFO` and the
  same option is covered by `selection.shift-drag`.
- `overlay-close.keyboard`: `?` opens the help overlay, Escape closes it.
- `overlay.click-inside-stays-open`: a click on overlay text leaves it open **and** a click outside it dismisses, in the same session. The second half is what makes the first a gate: on its own, "still open" is also true when the app ignores the mouse entirely.
- `overlay.click-outside-closes`: a click on a list row (not covered by the overlay) closes it. Adjust if the design chooses otherwise.
- `app-enables-mouse-reporting`: tmux `#{mouse_any_flag}/#{mouse_sgr_flag}` or xterm.js `mouseTrackingMode` shows the app sent DECSET 1000 + 1006 (SGR). The app deliberately never sends 1002 or 1003, so xterm.js reports `vt200` rather than `drag` or `any`. Not run for `tmux-sgr`.

Targets (tab labels, task titles, overlay title/label, the wheel cap and burst size) are constants
at the top of `e2e.py`.

## Hermeticity

Each check gets a fresh project (`orch init --adapter shell`, 2 agents with `--command true`,
1 goal, 2 named tasks plus `FILLERS` unnamed ones so the list is long enough for `wheel.cap`) and a throwaway `HOME`/`XDG_*`, set only on `orch` processes. `orch tui` runs
with `--observe` so no orchestrator starts and nothing is dispatched. tmux uses socket
`-L orch-e2e` with `-f /dev/null`; the server, the web process and the browser are torn down at exit.
`orch tui` exits on pty hangup (a defect this harness found), so `kill-session`
suffices; the harness also SIGKILLs the pane process as a belt against a check that wedged it.
