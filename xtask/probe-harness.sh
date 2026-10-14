#!/usr/bin/env bash
#
# Mutation probes for the parity harness.
#
# An acceptance gate that cannot fail is worthless, so this introduces a known
# divergence, asserts the harness catches it, and reverts. Every probe below
# corresponds to a numbered requirement in PARITY.md.
#
# It found two real harness blind spots when first written:
#   * the js-yaml cross-parse compared `JSON.stringify` output, which turns a
#     js-yaml `Date` straight back into an ISO string and erased the exact
#     difference V32 exists to find;
#   * the tree comparison compared parsed values only, so a missing JSON
#     trailing newline (contract, §6.4) went unnoticed.
#
# WARNING: this edits tracked source files and reverts them with `git
# checkout`. Run it on a clean working tree, from the repository root.
#
#   ./xtask/probe-harness.sh
#
set -uo pipefail

cd "$(dirname "$0")/.."

# Each probe reverts **its own file** with `git checkout`, so that file is all
# that has to be clean. Demanding a clean tree globally made the suite
# unusable whenever anyone had unrelated work in flight in the shared main
# checkout — which is the normal state of this repo — and the pressure then is
# to commit someone else's edit to get the probes to run. Checked per probe,
# just before mutating.
require_clean() {
  if ! git diff --quiet -- "$1" || ! git diff --cached --quiet -- "$1"; then
    printf '  DIRTY FILE    %s\n' "$2"
    printf '                %s has uncommitted changes; a probe would revert them\n' "$1"
    # Counted separately from `missed`. A missed probe means the harness has a
    # blind spot and the gate should stop; a dirty target file means the
    # operator has work in progress, which is a different conversation and
    # must not be reported as the first one. (It was, briefly: the gate said
    # "8 detected, 1 missed" when the only problem was my own uncommitted
    # edit to the file a probe mutates.)
    dirty=$((dirty + 1))
    return 1
  fi
}

export TMPDIR="${TMPDIR:-/tmp}"
detected=0
missed=0
pending=0
dirty=0

# probe_pending <subcommand> <label> <file> <old> <new>
#
# A probe for a mask or a code path that no scenario exercises yet, because the
# command it needs does not exist. Such a probe cannot fail, and reporting it as
# MISSED would make the suite red for something that is not a defect — the same
# erosion a flaky test causes. So it is reported as pending while the Rust
# binary lacks <subcommand>, and runs for real the moment that lands. Dropping
# it instead would mean nobody writes it later.
probe_pending() {
  local subcommand="$1"; shift
  has_subcommand "$subcommand"
  case $? in
    0) probe "$@" ;;
    1)
      printf '  pending       %s  (no `orch %s` yet)\n' "$1" "$subcommand"
      pending=$((pending + 1))
      ;;
    *)
      printf '  FAILED        %s  (probe harness could not find the orch binary)\n' "$1"
      missed=$((missed + 1))
      ;;
  esac
}

# has_subcommand <subcommand>
#
# `<cmd> --help` exits **3** outside a project, because everything but init,
# update and doctor builds the container before Commander parses. So the probe
# has to happen inside one, or every subcommand looks absent and every probe
# above it looks pending — the same trap the harness's own capability probe
# fell into.
#
# The binary is wherever cargo put it, which is **not** `<repo>/target/debug`
# under `gate.sh` — that sets `CARGO_TARGET_DIR` to `target/gate` for a cold
# run. This was hard-coded, and the failure it caused is worth keeping in mind:
# the cold gate found no binary, so both §14.1/§13 mask probes reported
# `pending` ("no `orch status` yet") on a tree where `status` plainly exists,
# and the gate still passed because pending is not a failure. Before that it was
# worse than wrong — it was passing by running whatever stale `target/debug/orch`
# a previous warm build had left behind, i.e. grading one tree's masks with
# another tree's binary.
#
# Exit status is three-valued on purpose: 0 present, 1 absent, **2 no binary at
# all**. A missing binary is an infrastructure fault, and reporting it as
# "absent" would turn every probe below into a pending — a green gate that
# tested nothing, which is the failure mode this whole script exists to catch.
has_subcommand() {
  local root="$PWD" dir orch
  orch="${CARGO_TARGET_DIR:-$root/target}/debug/orch"
  if [ ! -x "$orch" ]; then
    printf 'probe harness: no orch binary at %s\n' "$orch" >&2
    printf '  (CARGO_TARGET_DIR=%s)\n' "${CARGO_TARGET_DIR:-<unset>}" >&2
    return 2
  fi
  dir=$(mktemp -d)
  (
    cd "$dir" || exit 1
    mkdir -p home
    export HOME="$dir/home" USERPROFILE="$dir/home"
    "$orch" init --name probe --adapter claude >/dev/null 2>&1 || exit 1
    eval "\"$orch\" $1 --help" >/dev/null 2>&1
  )
  local ok=$?
  rm -rf "$dir"
  return $ok
}

# probe <label> <file> <python-replacement-expression>
#
# The replacement is applied with Python so multi-line anchors work; an anchor
# that no longer matches is a hard error, not a silent pass.
probe() {
  local label="$1" file="$2" old="$3" new="$4"

  require_clean "$file" "$label" || return

  if ! python3 - "$file" "$old" "$new" <<'PY'
import sys, pathlib
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
p = pathlib.Path(path)
s = p.read_text()
if old not in s:
    sys.exit(f"anchor not found in {path}: {old[:70]!r}")
p.write_text(s.replace(old, new, 1))
PY
  then
    printf '  ANCHOR STALE  %s\n' "$label"
    missed=$((missed + 1))
    return
  fi

  # Probes mutate orch-cli, the domain/infra crates, or the harness itself,
  # so build both: a mutation that does not compile proves nothing.
  if ! cargo build -p orch-cli -p xtask >/dev/null 2>&1; then
    printf '  BUILD BROKE   %s\n' "$label"
    missed=$((missed + 1))
    git checkout -- "$file"
    cargo build -p orch-cli -p xtask >/dev/null 2>&1
    return
  fi

  # Compare against the baseline *set* of failing scenarios, not against
  # "anything failed". The corpus is written ahead of the implementation, so
  # some scenarios are legitimately red — grepping for FAILED would then match
  # the standing red bar and report every probe as detected, which would make
  # this suite the very thing it exists to prevent: a gate that cannot fail.
  local newly
  run_parity
  if [ "$parity_ran" -eq 0 ]; then
    printf '  HARNESS BROKE %s\n' "$label"
    missed=$((missed + 1))
    git checkout -- "$file"
    cargo build -p orch-cli -p xtask >/dev/null 2>&1
    return
  fi
  newly=$(comm -13 <(printf '%s\n' "$baseline") <(printf '%s\n' "$last_failing"))

  if [ -n "$newly" ]; then
    printf '  detected      %s  (%s)\n' "$label" "$(printf '%s' "$newly" | tr '\n' ' ')"
    detected=$((detected + 1))
  else
    printf '  ** MISSED **  %s\n' "$label"
    missed=$((missed + 1))
  fi

  git checkout -- "$file"
  cargo build -p orch-cli -p xtask >/dev/null 2>&1
}

# Run the harness, setting $last_failing (sorted names of failing scenarios)
# and $parity_ran (whether it got as far as reporting a summary).
#
# These are globals rather than a return value on purpose: `x=$(run_parity)`
# would run the function in a subshell, and anything it assigned would be
# discarded — which is exactly the bug that first made every probe report
# HARNESS BROKE. The distinction matters because "nothing failed" and "the
# harness never ran" are both an empty list, and only the first is a missed
# probe.
last_failing=""
parity_ran=0
run_parity() {
  local out
  # Every probe below mutates something the `cmd-*`, `walking-skeleton` and
  # `index-tie-order` scenarios cover; none needs the agent-driven `o7-*`
  # ones, which cost about a minute each and would make this gate take over
  # an hour now that they are runnable.
  out=$(cargo xtask parity --exclude o7- 2>&1)
  last_failing=$(printf '%s' "$out" \
    | sed -n 's/^  \([a-z0-9-]*\) \.\.\. FAILED.*/\1/p' | sort)
  if printf '%s' "$out" | grep -q 'scenario(s):'; then
    parity_ran=1
  else
    parity_ran=0
  fi
}

echo "building the baseline"
cargo build -p orch-cli -p xtask >/dev/null 2>&1
run_parity
if [ "$parity_ran" -eq 0 ]; then
  echo "refusing to run: the harness does not run on a clean tree" >&2
  exit 1
fi
baseline="$last_failing"
if [ -n "$baseline" ]; then
  # Expected while the corpus runs ahead of the CLI: these are the red bar
  # WP-4/WP-5 land against. A probe counts only if it adds to this set.
  echo "  baseline red bar: $(printf '%s' "$baseline" | tr '\n' ' ')"
fi

echo "running mutation probes"

probe 'YAML timestamp guard off (R1, V8, V32)' \
  crates/orch-infra/src/storage/fs_utils.rs \
  'atomic_write(path, &quote_ambiguous_scalars(&raw)).await' \
  'atomic_write(path, &raw).await'

probe 'JSON trailing newline dropped (§6.4)' \
  crates/orch-infra/src/storage/fs_utils.rs \
  "    content.push('\\n');" \
  ''

probe 'index save keeps position instead of pushing (§6.10, §14.4)' \
  crates/orch-infra/src/storage/index.rs \
  '    items.retain(|item| id_of(item) != id);' \
  ''

probe 'agt_creator stall timeout wrong (§6.11)' \
  crates/orch-domain/src/shop/mod.rs \
  'stall_timeout_ms: Some(300_000),' \
  'stall_timeout_ms: Some(600_000),'

probe 'agt_creator role prompt byte-altered (§9.1, V28)' \
  crates/orch-domain/src/shop/mod.rs \
  'pub const AGENT_CREATOR_ROLE: &str = include_str!("roles/agent-creator.md");' \
  'pub const AGENT_CREATOR_ROLE: &str = "Agent architect.";'

probe 'human success message reworded (§5, L0 stdout)' \
  crates/orch-cli/src/commands/task.rs \
  'print_success(styles, &format!("Created {} \"{}\"", task.id, task.title));' \
  'print_success(styles, &format!("Created task {} \"{}\"", task.id, task.title));'

probe 'init drops workspace-exclude (§6.11 file set)' \
  crates/orch-cli/src/commands/init.rs \
  'atomic_write(&paths.workspace_exclude_path(), WORKSPACE_EXCLUDE).await?;' \
  ''

probe 'table padding changed (§5.1)' \
  crates/orch-cli/src/commands/task.rs \
  '        &rows,
        2,
    );' \
  '        &rows,
        3,
    );'

# ── The §14.1 named exclusion ────────────────────────────────────────────
#
# `state.claimed` is masked in `status --json` comparisons, which means the
# harness deliberately stops comparing one path. A mask is a hole in the gate,
# so these two probes assert the hole is exactly the size it is supposed to be:
# widening it must be caught, and it must not have blinded the harness to
# `claimed` anywhere else.

probe 'the claimed mask spreads to every command (§14.1 scope)' \
  xtask/src/harness.rs \
  '        let subcommand = argv.iter().find(|arg| !arg.starts_with('"'"'-'"'"'));
        subcommand.map(String::as_str) == Some("status") && argv.iter().any(|arg| arg == "--json")' \
  '        let _ = argv;
        true'

probe_pending status \
  'the claimed mask swallows the whole state object (§14.1 depth)' \
  xtask/src/harness.rs \
  '        document
            .get_mut("state")?
            .as_object_mut()?
            .shift_remove("claimed")' \
  '        document.as_object_mut()?.shift_remove("state")'

# The §13 toolchain-row exclusion. Same reasoning as §14.1 above: masking one
# `doctor` row must not blind the harness to the rest of the command's output.
probe_pending doctor \
  'the toolchain mask spreads to every command (§13 scope)' \
  xtask/src/harness.rs \
  '        argv.iter()
            .find(|arg| !arg.starts_with('"'"'-'"'"'))
            .map(String::as_str)
            == Some("doctor")' \
  '        let _ = argv;
        true'

echo
summary="$detected detected, $missed missed"
[ "$pending" -gt 0 ] && summary="$summary, $pending pending on commands that do not exist yet"
[ "$dirty" -gt 0 ] && summary="$summary, $dirty skipped on an uncommitted target file"
echo "$summary"
if [ "$missed" -gt 0 ]; then
  echo "a missed probe means the harness has a blind spot — fix the harness, not the probe" >&2
  exit 1
fi
if [ "$dirty" -gt 0 ]; then
  echo "$dirty probe(s) could not run because their target file has uncommitted changes;" >&2
  echo "commit or stash that work and re-run — this is not a harness blind spot" >&2
  exit 1
fi
echo "every probe was detected"
