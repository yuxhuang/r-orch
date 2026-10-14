#!/usr/bin/env bash
# Clone and build the pinned TypeScript ORCH reference used as the parity oracle.
#
# Patches in parity/patches/ are applied after checkout and before the build —
# see that directory's README for the bar a patch has to clear. A stamp file
# records which ones went in and what was built from them, and
# `cargo xtask parity` refuses to run unless it matches, so an unpatched or
# stale oracle cannot quietly grade r-orch.
set -euo pipefail
PIN=c066dc013e98e08d753ce204e604e04621dd4167
HERE="$(cd "$(dirname "$0")" && pwd)"
DIR="$HERE/orch-ts"
PATCHES="$HERE/patches"
STAMP="$DIR/.parity-stamp"

if [ ! -d "$DIR/.git" ]; then
  git clone https://github.com/oxgeneral/ORCH "$DIR"
fi
cd "$DIR"
git fetch --depth 1 origin "$PIN" 2>/dev/null || true
# `--force` so a previous run's applied patches do not block the checkout; the
# patches are re-applied below, which is also what makes this idempotent.
git checkout -q --force "$PIN"

applied=()
for patch in "$PATCHES"/*.patch; do
  [ -e "$patch" ] || continue
  name=$(basename "$patch")
  if ! git apply --check "$patch" 2>/dev/null; then
    echo "FAILED to apply $name against $PIN — the pin moved, or the patch is stale." >&2
    echo "Fix the patch; do not skip it. See $PATCHES/README.md." >&2
    exit 1
  fi
  git apply "$patch"
  applied+=("$name")
  echo "applied $name"
done

npm ci --ignore-scripts
# A clean dist so the stamp hashes this build and not a previous one's leftover
# chunks; the chunk names are content-hashed, so stale files accumulate.
rm -rf dist
npm run build

# The stamp is keyed to the built output, so rebuilding without the patches —
# `npm run build` straight after a plain `git checkout` — invalidates it rather
# than leaving a stamp that lies.
#
# A hash of the per-file hashes, sorted under LC_ALL=C. The obvious
# `cat dist/*.js | sha256sum` leaves the concatenation order to the shell's
# collation, and the harness verifies this value from a *different* shell --
# which produced a real mismatch and would have failed the gate for a reason
# that has nothing to do with the oracle. A sorted manifest of per-file
# hashes has no ordering question to get wrong.
hash=$(find dist -maxdepth 1 -name '*.js' -exec sha256sum {} + \
  | LC_ALL=C sort -k 2 | sha256sum | cut -d' ' -f1)
{
  printf 'pin %s\n' "$PIN"
  for name in ${applied+"${applied[@]}"}; do printf 'patch %s\n' "$name"; done
  printf 'dist-sha256 %s\n' "$hash"
} > "$STAMP"

echo "reference built: $DIR/dist/cli.js"
echo "stamp: $STAMP ($((${#applied[@]})) patch(es), dist ${hash:0:12})"
