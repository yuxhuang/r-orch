#!/usr/bin/env bash
# Clone and build the pinned TypeScript ORCH reference used as the parity oracle.
set -euo pipefail
PIN=c066dc013e98e08d753ce204e604e04621dd4167
DIR="$(cd "$(dirname "$0")" && pwd)/orch-ts"
if [ ! -d "$DIR/.git" ]; then
  git clone https://github.com/oxgeneral/ORCH "$DIR"
fi
cd "$DIR"
git fetch --depth 1 origin "$PIN" 2>/dev/null || true
git checkout -q "$PIN"
npm ci --ignore-scripts
npm run build
echo "reference built: $DIR/dist/cli.js"
