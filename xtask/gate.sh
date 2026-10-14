#!/usr/bin/env bash
#
# From-scratch verification, for the review gate.
#
# Everything here is runnable piecemeal during development; the point of this
# script is that it runs against a **cold target directory**, so nothing passes
# because of a stale artifact. That matters more than usual in this repo: the
# work packages shared one `CARGO_TARGET_DIR` across git worktrees for a while,
# artifacts from two trees hash identically, and a parity run was once observed
# grading a different branch's binary with entirely plausible diffs.
#
#   ./xtask/gate.sh              # the full gate
#   ./xtask/gate.sh --warm       # reuse the normal target dir (fast, not a gate)
#   ./xtask/gate.sh --keep       # leave target/gate behind to inspect
#
# Exit status is 0 only if every step passed. The summary table is printed
# whatever happens, including on an early failure, because "which step failed"
# is the first thing anyone asks.
set -uo pipefail

cd "$(dirname "$0")/.."
REPO=$(pwd)

WARM=0
KEEP=0
for arg in "$@"; do
  case "$arg" in
    --warm) WARM=1 ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# A cold build of this workspace with --all-targets is a few GB. Refuse rather
# than filling the disk and failing halfway through something unrelated.
if [ "$WARM" -eq 0 ]; then
  FREE_GB=$(df -BG --output=avail "$REPO" | tail -1 | tr -dc '0-9')
  if [ "${FREE_GB:-0}" -lt 12 ]; then
    echo "refusing to run a cold gate: only ${FREE_GB}G free, and a fresh" >&2
    echo "target directory needs roughly 8-10G. Free some space, or use" >&2
    echo "--warm to reuse the existing target dir (faster, but not a gate)." >&2
    exit 1
  fi
  export CARGO_TARGET_DIR="$REPO/target/gate"
  # Inside the repo's own target/ on purpose: it is already gitignored, it is
  # on the same filesystem, and nothing else in the project may create a
  # second top-level target directory.
  rm -rf "$CARGO_TARGET_DIR"
  mkdir -p "$CARGO_TARGET_DIR"
  cleanup() {
    if [ "$KEEP" -eq 1 ]; then
      echo "leaving $CARGO_TARGET_DIR in place (--keep)"
    else
      rm -rf "$CARGO_TARGET_DIR"
    fi
  }
  # Removed even when the gate fails or is interrupted — an abandoned cold
  # target dir is several GB of surprise.
  trap cleanup EXIT INT TERM
  MODE="cold ($CARGO_TARGET_DIR)"
else
  MODE="warm (reusing $REPO/target)"
fi

declare -a NAMES=() RESULTS=() DETAILS=()
FAILED=0

# step <name> <command…> — run it, record pass/fail and a one-line detail.
#
# The detail is pulled out of the step's own output by `summarize`, so the
# table says "1559 passed" rather than just "ok" and the summary is worth
# reading on its own.
step() {
  local name="$1"; shift
  printf '\n════ %s\n' "$name"
  local log; log=$(mktemp "${TMPDIR:-/tmp}/gate-XXXXXX")
  local rc=0
  if "$@" >"$log" 2>&1; then rc=0; else rc=$?; fi
  tail -40 "$log"
  NAMES+=("$name")
  DETAILS+=("$(summarize "$name" "$log")")
  if [ "$rc" -eq 0 ]; then
    RESULTS+=("pass")
  else
    RESULTS+=("FAIL ($rc)")
    FAILED=$((FAILED + 1))
  fi
  rm -f "$log"
}

summarize() {
  local name="$1" log="$2"
  case "$name" in
    test)
      # Name the failures as well as counting them. A cold gate run is the
      # only place a timing-sensitive test has shown up here, and "1 failed"
      # with the log already deleted is not enough to chase it.
      local counts failures
      counts=$(awk '/^test result:/ { p += $4; f += $6 } END { printf "%d passed, %d failed", p, f }' "$log")
      failures=$(sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$log" | sort -u | paste -sd' ' -)
      if [ -n "$failures" ]; then
        printf '%s — %s' "$counts" "$failures"
      else
        printf '%s' "$counts"
      fi
      ;;
    parity)
      # Name the failures, not just the count: this gate runs with a standing
      # red bar (a known contract gap the CLI has not closed yet), and the
      # operator needs to tell that apart from a new regression without
      # scrolling back through the log.
      local counts failures
      counts=$(grep -oE '[0-9]+ scenario\(s\): .*' "$log" | tail -1)
      failures=$(sed -n 's/^  \([a-z0-9-]*\) \.\.\. FAILED.*/\1/p' "$log" | paste -sd' ' -)
      if [ -n "$failures" ]; then
        printf '%s — %s' "$counts" "$failures"
      else
        printf '%s' "$counts"
      fi
      ;;
    lint-scenarios)
      grep -oE 'all [0-9]+ scenario\(s\) well-formed|[0-9]+ of [0-9]+ scenario\(s\) are malformed' "$log" | tail -1
      ;;
    probes)
      # The whole summary line, so "skipped on an uncommitted target file"
      # reads as that rather than as a blind spot.
      grep -oE '[0-9]+ detected, [0-9]+ missed.*' "$log" | tail -1
      ;;
    crate-graph)
      grep -oE 'layering matches [^ ]+ .*|[0-9]+ layering problem\(s\)' "$log" | tail -1
      ;;
    fmt)
      # `cargo fmt --check` prints a diff, not diagnostics, so count the
      # files it would rewrite rather than error lines.
      local n; n=$(grep -cE '^Diff in ' "$log")
      printf '%d file(s) need reformatting' "$n"
      ;;
    clippy|build)
      local n; n=$(grep -cE '^(error|warning)' "$log")
      printf '%d error/warning line(s)' "$n"
      ;;
    *) printf -- '-' ;;
  esac
}

printf 'r-orch gate — %s\n' "$MODE"
printf 'commit %s\n' "$(git rev-parse --short HEAD)"
if ! git diff --quiet || ! git diff --cached --quiet; then
  printf 'WARNING: the working tree is dirty; this gates the tree, not the commit\n'
fi

step build          cargo build --workspace --all-targets
step test           cargo test --workspace --no-fail-fast
step clippy         cargo clippy --workspace --all-targets -- -D warnings
step fmt            cargo fmt --all --check
step crate-graph    cargo xtask crate-graph
step lint-scenarios cargo xtask lint-scenarios
step parity         cargo xtask parity
# The probe suite rebuilds the CLI repeatedly as it mutates and reverts source,
# so it goes last: on a cold gate it is by far the slowest step, and anything
# it would catch is worth knowing about after the cheaper checks have run.
step probes         ./xtask/probe-harness.sh

printf '\n'
printf '════════════════════════════════════════════════════════════════════\n'
printf ' r-orch gate — %s\n' "$MODE"
printf '════════════════════════════════════════════════════════════════════\n'
for i in "${!NAMES[@]}"; do
  printf ' %-9s %-16s %s\n' "${RESULTS[$i]}" "${NAMES[$i]}" "${DETAILS[$i]}"
done
printf '════════════════════════════════════════════════════════════════════\n'

if [ "$FAILED" -gt 0 ]; then
  printf ' %d of %d step(s) FAILED\n' "$FAILED" "${#NAMES[@]}"
  exit 1
fi
printf ' all %d step(s) passed\n' "${#NAMES[@]}"
