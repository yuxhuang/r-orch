# Shared helpers for the fake agent CLIs. Sourced, never executed.
#
# POSIX sh only: the differential harness must spawn no `npm` and make no
# network call (PARITY.md V31), and both binaries have to run these.

set -u

# ── Scenario selection ──────────────────────────────────────────────────────
#
# `ORCH_FAKE_SCENARIO` sets it globally; a marker in the prompt overrides that,
# which is how a scenario drives one task differently from another through the
# rendered prompt alone.
fake_scenario() {
  prompt="${1-}"
  scenario="${ORCH_FAKE_SCENARIO:-success}"
  case "$prompt" in
    *ORCH_FAKE_FAIL*)         scenario=fail ;;
    *ORCH_FAKE_RATE_LIMIT*)   scenario=rate_limit ;;
    *ORCH_FAKE_STDERR_FLOOD*) scenario=stderr_flood ;;
    *ORCH_FAKE_SLOW*)         scenario=slow ;;
    *ORCH_FAKE_CRASH*)        scenario=crash ;;
    *ORCH_FAKE_SILENT*)       scenario=silent ;;
  esac
  printf '%s' "$scenario"
}

# The reply text an agent "produces". Deterministic, so the differential
# harness can compare run artefacts byte for byte.
fake_reply() {
  printf '%s' "${ORCH_FAKE_REPLY:-ORCH_FAKE_OK}"
}

# ── Scenario effects, shared by every adapter ───────────────────────────────

# 256 KiB on stderr: more than one pipe buffer, so an adapter that fails to
# drain it deadlocks (PARITY.md §14.12).
fake_flood_stderr() {
  i=0
  while [ "$i" -lt 2048 ]; do
    printf 'fake-agent stderr noise %s %s\n' "$i" \
      'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' >&2
    i=$((i + 1))
  done
}

# Everything a scenario does *before* the adapter-specific output.
fake_before_output() {
  case "$1" in
    stderr_flood) fake_flood_stderr ;;
    slow)         sleep "${ORCH_FAKE_SLOW_SECONDS:-30}" ;;
    crash)        kill -9 $$ ;;
  esac
}

# The exit status for a scenario, after its output has been written.
fake_exit_code() {
  case "$1" in
    fail|rate_limit) printf '%s' "${ORCH_FAKE_EXIT_CODE:-1}" ;;
    *)               printf '0' ;;
  esac
}

# The stderr line a failing scenario leaves behind, which is what the
# adapters' bounded tail picks up and classifies.
fake_failure_stderr() {
  case "$1" in
    fail)       printf 'fake-agent: the task failed\n' >&2 ;;
    rate_limit) printf 'fake-agent: API rate limit reached\n' >&2 ;;
  esac
}

# ── Argument helpers ────────────────────────────────────────────────────────

# `--version` short-circuits every CLI: `orch doctor` probes it.
fake_handle_version() {
  for arg in "$@"; do
    if [ "$arg" = "--version" ]; then
      printf '%s\n' "${ORCH_FAKE_VERSION:-0.0.0-fake}"
      exit 0
    fi
  done
}

# The last positional argument, which is where claude, cursor and opencode put
# the prompt.
fake_last_arg() {
  prompt=""
  for arg in "$@"; do
    prompt="$arg"
  done
  printf '%s' "$prompt"
}

# The value following `$1` in the remaining arguments — `-p <prompt>` for grok
# and antigravity.
fake_value_after() {
  wanted="$1"
  shift
  found=""
  for arg in "$@"; do
    if [ -n "$found" ]; then
      printf '%s' "$arg"
      return 0
    fi
    if [ "$arg" = "$wanted" ]; then
      found=1
    fi
  done
  printf ''
}

# JSON-escape a string for the JSONL formats: backslashes, quotes, newlines
# and tabs. `printf '%s'` keeps it byte-exact otherwise.
fake_json_string() {
  # Join lines with a literal \n in awk: the sed `:a;N;$!ba` idiom is GNU-only,
  # and BSD sed prints nothing for one-line input.
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/	/\\t/g' |
    awk 'NR > 1 { printf "\\n" } { printf "%s", $0 }'
}
