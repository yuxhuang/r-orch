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

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "refusing to run: the working tree has uncommitted changes, and probes" >&2
  echo "revert with \`git checkout\`. Commit or stash first." >&2
  exit 1
fi

export TMPDIR="${TMPDIR:-/tmp}"
detected=0
missed=0

# probe <label> <file> <python-replacement-expression>
#
# The replacement is applied with Python so multi-line anchors work; an anchor
# that no longer matches is a hard error, not a silent pass.
probe() {
  local label="$1" file="$2" old="$3" new="$4"

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
  local now newly
  now=$(failing_scenarios)
  if ! parity_ran; then
    printf '  HARNESS BROKE %s\n' "$label"
    missed=$((missed + 1))
    git checkout -- "$file"
    cargo build -p orch-cli -p xtask >/dev/null 2>&1
    return
  fi
  newly=$(comm -13 <(printf '%s\n' "$baseline") <(printf '%s\n' "$now"))

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

# The sorted names of every scenario the harness currently reports as failing.
#
# The raw run goes to a **file**, not a variable: every caller uses
# `$(failing_scenarios)`, which is a subshell, so an assignment inside would
# never reach the parent — `parity_ran` would then be false for every probe and
# the whole suite would report HARNESS BROKE without evaluating anything.
# Writing it out lets the caller tell "every scenario passed" from "the harness
# never got as far as reporting", which otherwise look identical: an empty list.
last_parity_output=$(mktemp "${TMPDIR%/}/probe-harness-parity.XXXXXX")
trap 'rm -f "$last_parity_output"' EXIT
failing_scenarios() {
  cargo xtask parity >"$last_parity_output" 2>&1
  sed -n 's/^  \([a-z0-9-]*\) \.\.\. FAILED.*/\1/p' "$last_parity_output" | sort
}

# Whether the harness actually ran to completion.
parity_ran() {
  grep -q 'scenario(s):' "$last_parity_output"
}

echo "building the baseline"
cargo build -p orch-cli -p xtask >/dev/null 2>&1
baseline=$(failing_scenarios)
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

probe 'the claimed mask swallows the whole state object (§14.1 depth)' \
  xtask/src/harness.rs \
  '        document
            .get_mut("state")?
            .as_object_mut()?
            .shift_remove("claimed")' \
  '        document.as_object_mut()?.shift_remove("state")'

echo
echo "$detected detected, $missed missed"
if [ "$missed" -gt 0 ]; then
  echo "a missed probe means the harness has a blind spot — fix the harness, not the probe" >&2
  exit 1
fi
echo "every probe was detected"
