#!/usr/bin/env bash
# Regenerate docs/manual/14-command-reference.md from `orch <command> --help`.
#
#   docs/manual/tools/gen-command-reference.sh [path/to/orch]
#
# Subcommand help only works inside a project, so this runs `orch init` in a
# throwaway directory under the system temp dir, with its own HOME, and deletes
# it on exit. It never reads or writes a real project or home directory.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
manual="$(dirname "$here")"
orch="${1:-$(cd "$manual/../.." && pwd)/target/debug/orch}"
[ -x "$orch" ] || { echo "orch binary not found: $orch (build it or pass its path)" >&2; exit 1; }

sandbox="$(mktemp -d "${TMPDIR:-/tmp}/orch-cmdref.XXXXXX")"
trap 'rm -rf "$sandbox"' EXIT
# orch walks up to the nearest .orchestry/, `orch init` commits into any enclosing git work tree, and
# ~/.orchestry is orch's global directory: a sandbox under HOME could act on a real project.
real_home="$(cd "$HOME" && pwd -P)"
if [[ "$(cd "$sandbox" && pwd -P)/" == "$real_home"/* ]]; then
  echo "refusing: temp dir $sandbox is under your home directory; set TMPDIR to a directory outside it" >&2
  exit 1
fi
mkdir -p "$sandbox/home" "$sandbox/project"
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export HOME="$sandbox/home" ORCHESTRY_GLOBAL_CONFIG_PATH="$sandbox/home/global.yml" NO_COLOR=1
cd "$sandbox/project"
"$orch" init >/dev/null
[ -d .orchestry ] || { echo "orch init did not create .orchestry/ in $PWD" >&2; exit 1; }

dump="$sandbox/help.txt"
walk() {
  echo "=================== orch $*"
  "$orch" "$@" --help 2>&1
  "$orch" "$@" --help 2>&1 |
    awk '/^Commands:/{f=1;next} /^[A-Z][a-z]+:/{f=0} f && $1!="help" && $1!="" {print $1}' |
    while read -r sub; do walk "$@" "$sub"; done
}
walk > "$dump"

python3 -I "$here/command_reference.py" "$dump" "$manual/14-command-reference.md"
