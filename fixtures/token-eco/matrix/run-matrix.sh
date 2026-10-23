#!/bin/sh
# Old/new compatibility matrix (design-v3 D5-1 / D5-7, token_eco_matrix.rs and
# token_eco_effort_diagnostic_isolation.rs).
#
# Builds (or reuses) the d8c2319 baseline `orch` in a DISPOSABLE export, then
# runs the matrix tests against the workspace's own `orch` with the baseline as
# the old reader. The shared checkout is never reset.
#
#   fixtures/token-eco/matrix/run-matrix.sh [PREBUILT_OLD_ORCH]
#
# Env: BASE_REV (default d8c2319), WORK (default a new mktemp dir under /tmp),
#      CARGO_TARGET_DIR for the NEW build (default: the workspace's).
# Every temporary store is created by the tests themselves (tempfile).
set -eu
repo=$(cd "$(dirname "$0")/../../.." && pwd)
rev=${BASE_REV:-d8c2319}
work=${WORK:-$(mktemp -d /tmp/token-eco-matrix.XXXXXX)}
old=${1:-}
if [ -z "$old" ]; then
  mkdir -p "$work/src"
  git -C "$repo" archive "$rev" | tar -x -C "$work/src"
  (cd "$work/src" && CARGO_TARGET_DIR="$work/target" cargo build -p orch-cli --bin orch)
  old="$work/target/debug/orch"
fi
[ -x "$old" ] || { echo "old orch binary $old not executable" >&2; exit 2; }
echo "OLD_BIN=$old"
echo "OLD_SHA256=$(shasum -a 256 "$old" | cut -d' ' -f1)"
export ORCH_T1_OLD_BIN="$old" ORCH_T1_REQUIRE_OLD=1
cd "$repo"
cargo build -p orch-cli --bin orch
echo "NEW_SHA256=$(shasum -a 256 "${CARGO_TARGET_DIR:-$repo/target}/debug/orch" | cut -d' ' -f1)"
cargo test -p orch-cli --test token_eco_matrix -- --nocapture --test-threads=4
cargo test -p orch-cli --test token_eco_effort_diagnostic_isolation -- --include-ignored --nocapture
