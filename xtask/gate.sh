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
# Logs of failing steps land here. Inside target/, so it is gitignored and a
# cold run's cleanup does not take it: the point is to survive the run.
LOG_KEEP="$REPO/target/gate-logs"
rm -rf "$LOG_KEEP"

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
    rm -f "$log"
  else
    RESULTS+=("FAIL ($rc)")
    FAILED=$((FAILED + 1))
    # Keep the log. A failing step's output is the only evidence of *why*, and
    # deleting it has already cost two investigations: an intermittent test
    # failed under the cold gate's I/O load, and by the time anyone looked
    # there was nothing but "1 failed" to go on. Cheap insurance.
    mkdir -p "$LOG_KEEP"
    cp "$log" "$LOG_KEEP/$name.log"
    rm -f "$log"
    printf '  (log kept at %s/%s.log)\n' "$LOG_KEEP" "$name"
  fi
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
    no-real-cli)
      grep -oE "no test exec'd an agent CLI|a test exec'd a real agent CLI:" "$log" | tail -1
      ;;
    real-home)
      grep -oE 'still does not exist|existed before this run.*|was created during this gate run.*' "$log" | tail -1
      ;;
    home-hygiene)
      grep -oE 'a sentinel HOME is still empty.*|the suite wrote into a sentinel HOME:' "$log" | tail -1
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

# Run the suite with HOME pointed at an empty directory and fail if **anything**
# appears in it.
#
# Originally this only looked for `.orchestry`, which let the rest through: the
# adapter and FTUE tests spawned the real `cursor` and `opencode`, which wrote
# `.cursor/cli-config.json`, `.cache/opencode`, `.config/opencode` and
# `.local/share/opencode`. Same standard as ORCH data — no test may write into
# a developer's home — so the check is now "the directory is still empty".
#
# `.cargo` and `.rustup` are the one exception, and only because *cargo itself*
# recreates them when HOME moves: they are the build tool's own state, created
# before any test code runs. Everything else is a finding.
#
# Runs from **outside** the workspace. An earlier version ran with the cwd in
# the repo and passed while a bug was live, because cargo only loads
# `.cargo/config.toml` — and its `[env]` — from inside the workspace, so the
# redirect that was hiding the writer was still in effect. CARGO_HOME and
# RUSTUP_HOME are set explicitly because a moved HOME otherwise hides the
# toolchain, and the suite then runs **zero tests** while this reports clean.
sentinel_home_check() {
  local sentinel cwd unexpected status
  sentinel=$(mktemp -d "${TMPDIR:-/tmp}/r-orch-sentinel-home-XXXXXX")
  cwd=$(mktemp -d "${TMPDIR:-/tmp}/r-orch-sentinel-cwd-XXXXXX")

  (
    cd "$cwd" || exit 1
    HOME="$sentinel" \
    CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}" \
    RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}" \
    RUSTUP_TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$REPO/rust-toolchain.toml")" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}" \
      cargo test --manifest-path "$REPO/Cargo.toml" --workspace --no-fail-fast
  ) >/dev/null 2>&1

  unexpected=$(find "$sentinel" -mindepth 1 -maxdepth 1 2>/dev/null \
               | grep -vE '/\.(cargo|rustup)$' || true)
  status=0
  if [ -n "$unexpected" ]; then
    echo "the suite wrote into a sentinel HOME:"
    printf '%s\n' "$unexpected" | sed "s|$sentinel|<HOME>|" | head -12
    status=1
  else
    echo "a sentinel HOME is still empty after the suite"
  fi
  rm -rf "$sentinel" "$cwd"
  return "$status"
}

# Run the suite with the AI CLIs replaced by shims that record being called,
# and report anything that execs one.
#
# `cargo test` should never exec a real agent CLI: the adapter tests drive a
# scripted `FakeProcessManager`, and the fixtures under fixtures/fake-agents
# are for `cargo xtask parity`. One test did anyway — it ran `claude --help`,
# `cursor-agent --list-models` and read the real `~/.codex` — which is both a
# hermeticity hole (results depend on what is installed) and a privacy one.
#
# A shim rather than an empty PATH, because an absent binary is indistinguishable
# from one that was never wanted: the shim names the caller. The suite's own
# result is ignored here — a test that expected a working CLI may now fail, and
# that is not what this step is asking. Pass/fail of the suite is the `test`
# step's job.
no_real_cli_check() {
  # `name` is declared local here on purpose: bash scopes dynamically, so the
  # loop below would otherwise overwrite `step`'s own `local name` and the
  # summary table would label this row `agy` — which it did.
  local shims log name status others sentinel cwd
  shims=$(mktemp -d "${TMPDIR:-/tmp}/r-orch-shims-XXXXXX")
  log="$shims/calls.log"
  : > "$log"

  for name in claude codex cursor cursor-agent agent opencode pi grok agy; do
    cat > "$shims/$name" <<SHIM
#!/bin/sh
printf '%s %s\n' "$name" "\$*" >> "$log"
exit 127
SHIM
    chmod +x "$shims/$name"
  done

  sentinel=$(mktemp -d "${TMPDIR:-/tmp}/r-orch-shim-home-XXXXXX")
  cwd=$(mktemp -d "${TMPDIR:-/tmp}/r-orch-shim-cwd-XXXXXX")
  (
    cd "$cwd" || exit 1
    HOME="$sentinel" \
    PATH="$shims:$PATH" \
    CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}" \
    RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}" \
    RUSTUP_TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$REPO/rust-toolchain.toml")" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}" \
      cargo test --manifest-path "$REPO/Cargo.toml" --workspace --no-fail-fast
  ) >/dev/null 2>&1

  status=0
  if [ -s "$log" ]; then
    echo "a test exec'd a real agent CLI:"
    sort "$log" | uniq -c | sort -rn | head -10
    status=1
  else
    echo "no test exec'd an agent CLI"
  fi
  rm -rf "$shims" "$sentinel" "$cwd"
  return "$status"
}

# The real home must be exactly as we found it.
#
# The sentinel checks above prove the suite is hermetic under a *moved* HOME;
# this proves nothing slipped past them into the actual one. Cheap, and it is
# the property the user actually cares about.
# Captured before anything moves HOME.
REAL_HOME="${HOME:-/nonexistent}"
REAL_ORCHESTRY="$REAL_HOME/.orchestry"
REAL_ORCHESTRY_EXISTED=0
[ -e "$REAL_ORCHESTRY" ] && REAL_ORCHESTRY_EXISTED=1

real_home_untouched_check() {
  if [ "$REAL_ORCHESTRY_EXISTED" -eq 0 ] && [ -e "$REAL_ORCHESTRY" ]; then
    echo "$REAL_ORCHESTRY was created during this gate run:"
    find "$REAL_ORCHESTRY" 2>/dev/null | head -20
    return 1
  fi
  if [ "$REAL_ORCHESTRY_EXISTED" -eq 1 ]; then
    echo "$REAL_ORCHESTRY existed before this run; left alone"
    return 0
  fi
  echo "$REAL_ORCHESTRY still does not exist"
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
step home-hygiene   sentinel_home_check
step no-real-cli    no_real_cli_check
step probes         ./xtask/probe-harness.sh
# Last, so it covers every step above it — including the probe suite, which
# rebuilds and re-runs the CLI repeatedly.
step real-home      real_home_untouched_check

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
