#!/usr/bin/env bash
# Single entry point for the TUI mouse end-to-end harness. See README.md.
#   run.sh [--no-build] [--only ENV[,ENV...]]    ENV: tmux-sgr tmux-fwd xterm-web
# Exit status is non-zero if any PASS/FAIL line is FAIL.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

build=1
args=()
for a in "$@"; do
  if [ "$a" = "--no-build" ]; then build=0; else args+=("$a"); fi
done

# Do NOT touch HOME here: rustup would re-download the toolchain. e2e.py sets
# HOME only on the `orch` processes it spawns.
if [ "$build" = 1 ]; then
  (cd "$repo" && cargo build -p orch-cli >&2)
fi
export ORCH_BIN="${ORCH_BIN:-$repo/target/debug/orch}"
[ -x "$ORCH_BIN" ] || { echo "ORCH_BIN not found: $ORCH_BIN" >&2; exit 2; }

for t in tmux python3; do command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 2; }; done

# Browser path: Playwright from a Python venv (override with PW_PYTHON).
export PW_PYTHON="${PW_PYTHON:-.venv/bin/python}"
# PLAYWRIGHT_BROWSERS_PATH is honoured if set; otherwise Playwright's default cache is used.

# Run under the Playwright venv python when present (stdlib suffices for the tmux
# environments; the xterm-web environment is reported as a FAIL if playwright is absent).
py=python3
[ -x "$PW_PYTHON" ] && py="$PW_PYTHON"
exec "$py" -I "$here/e2e.py" ${args[@]+"${args[@]}"}
