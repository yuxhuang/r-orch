#!/usr/bin/env bash
# Bootstrap the tiered gate. full (default) is the goal/release gate; pr is the
# merge condition after the policy transition; fast belongs to PR2.
#
# Incident protections moved to the Rust gate (plan incident ledger I1-I24):
# separate targets per worktree; cold-target admission and 12GB refusal; retained
# step logs, real statuses, commit/tree and timings; one judged sentinel/shim
# suite from outside the workspace; nonzero counts and environment witness;
# metadata-only real-home comparison last; missing advisories is ENV-LIMITED.
# No wrapper removes a build target at startup, EXIT, INT, TERM or HUP. The Rust
# owner alone may finish a use. Native D45 mutation is currently unavailable:
# actual tier execution REFUSES. This partial implementation is not a release
# gate replacement or merge evidence. The historical gate remains at the base SHA.
#
# Bootstrap freshness is conservative while native reset is blocked: use a new
# private target per invocation under target/gate-xtask, never another worktree's
# CARGO_TARGET_DIR or a stamped old executable. These directories are retained;
# bounded retention/reuse awaits the native lifecycle decision. No timing claim.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$(pwd -P)
export LC_ALL=C
# Refuse source links: git hashes link text while rustc follows the target.
paths=$(mktemp "${TMPDIR:-/tmp}/gate-judge-paths.XXXXXX")
ids=$(mktemp "${TMPDIR:-/tmp}/gate-judge-ids.XXXXXX")
git ls-files -z --cached --others --exclude-standard -- xtask Cargo.toml Cargo.lock rust-toolchain.toml .cargo/config.toml > "$paths"
while IFS= read -r -d '' path; do
  case "$path" in
    *$'\n'*|*$'\r'*|*$'\t'*) echo 'REFUSED: unsupported judge source path' >&2; exit 1 ;;
  esac
  parent="$path"
  while [ "$parent" != . ]; do
    if [ -L "$parent" ]; then echo 'REFUSED: judge source link' >&2; exit 1; fi
    parent=$(dirname "$parent")
  done
  [ -f "$path" ] || { echo 'REFUSED: judge source is not a regular file' >&2; exit 1; }
  printf '%s\t%s\n' "$path" "$(git hash-object --no-filters -- "$path")" >> "$ids"
done < "$paths"
JUDGE=$(sort -t $'\t' -k1,1 "$ids" | while IFS=$'\t' read -r path blob; do printf '%s %s\n' "$blob" "$path"; done | git hash-object --stdin)
# Identity scratch contains no child resources. Nothing in a target is deleted.
rm -f "$paths" "$ids"
if [ ! -e target ]; then mkdir target; fi
[ -d target ] && [ ! -L target ] || { echo 'REFUSED: target directory link/special' >&2; exit 1; }
if [ ! -e target/gate-xtask ]; then mkdir target/gate-xtask; fi
[ -d target/gate-xtask ] && [ ! -L target/gate-xtask ] || { echo 'REFUSED: bootstrap directory link/special' >&2; exit 1; }
BOOTSTRAP=$(mktemp -d "$REPO/target/gate-xtask/build.XXXXXX")
export CARGO_TARGET_DIR="$BOOTSTRAP"
# rustup reads this checkout's pinned toolchain. No Homebrew/system cargo bypass.
rustup which cargo >/dev/null
cargo build --locked --quiet -p xtask
# A source edit during bootstrap cannot qualify the resulting judge.
export XTASK_GATE_JUDGE_ID="$JUDGE"
exec "$BOOTSTRAP/debug/xtask" gate "$@"
