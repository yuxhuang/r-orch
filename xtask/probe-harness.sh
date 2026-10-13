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

  if ! cargo build -p orch-cli >/dev/null 2>&1; then
    # A mutation that does not compile proves nothing about the harness.
    printf '  BUILD BROKE   %s\n' "$label"
    missed=$((missed + 1))
    git checkout -- "$file"
    cargo build -p orch-cli >/dev/null 2>&1
    return
  fi

  # Capture before grepping: `pipefail` would otherwise make the condition
  # reflect `cargo`'s non-zero exit (the harness bails on failure) rather than
  # grep's match, inverting every result.
  local out
  out=$(cargo xtask parity 2>&1)
  if printf '%s' "$out" | grep -q FAILED; then
    printf '  detected      %s\n' "$label"
    detected=$((detected + 1))
  else
    printf '  ** MISSED **  %s\n' "$label"
    missed=$((missed + 1))
  fi

  git checkout -- "$file"
  cargo build -p orch-cli >/dev/null 2>&1
}

echo "building the baseline"
cargo build -p orch-cli >/dev/null 2>&1
baseline=$(cargo xtask parity 2>&1)
if printf '%s' "$baseline" | grep -q FAILED; then
  echo "refusing to run: the harness is already red on a clean tree" >&2
  printf '%s\n' "$baseline" >&2
  exit 1
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

echo
echo "$detected detected, $missed missed"
if [ "$missed" -gt 0 ]; then
  echo "a missed probe means the harness has a blind spot — fix the harness, not the probe" >&2
  exit 1
fi
echo "every probe was detected"
