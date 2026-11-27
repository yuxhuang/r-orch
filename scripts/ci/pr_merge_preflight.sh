#!/usr/bin/env bash
# Fail closed before classification or builds. This checks this checkout, not
# the event's potentially stale base/head fields or a moving merge ref.
set -euo pipefail

refuse() {
  echo "::error::PR merge preflight: $*" >&2
  exit 1
}

merge=$(git rev-parse --verify HEAD)
parents=$(git show -s --format=%P "$merge")
printf 'observed merge=%s parents=[%s]\n' "$merge" "$parents"
echo 'expected base=<indeterminate> head=<indeterminate> (live lookup pending)'

[[ ${PR_NUMBER:-} =~ ^[1-9][0-9]*$ ]] || refuse 'invalid PR number'
base_ref="refs/heads/${PR_BASE_REF:?missing base ref}"
head_ref="refs/pull/$PR_NUMBER/head"
git check-ref-format "$base_ref" || refuse 'invalid base ref'

# A single bounded advertisement reads the target branch and the upstream PR
# head ref (also for forks). A failed command never yields usable evidence,
# even when it printed both refs before failing. No token is persisted.
if ! refs=$(GIT_TERMINAL_PROMPT=0 timeout --kill-after=5s 20s git ls-remote --refs origin "$base_ref" "$head_ref"); then
  refuse 'live ref lookup failed or timed out; expected pair indeterminate'
fi
expected_base=
expected_head=
while read -r sha ref extra; do
  [[ $sha =~ ^[0-9a-f]{40}$ && -z $extra ]] || refuse 'malformed live ref'
  case "$ref" in
    "$base_ref")
      [[ -z $expected_base ]] || refuse 'duplicate live base'
      expected_base=$sha ;;
    "$head_ref")
      [[ -z $expected_head ]] || refuse 'duplicate live head'
      expected_head=$sha ;;
    *) refuse 'unexpected live ref' ;;
  esac
done <<< "$refs"
printf 'expected base=%s head=%s\n' "${expected_base:-<indeterminate>}" "${expected_head:-<indeterminate>}"
[[ -n $expected_base && -n $expected_head ]] || refuse 'live ref missing; expected pair indeterminate'
# String equality enforces exactly two parents, in base/head order.
[[ $parents == "$expected_base $expected_head" ]] || refuse 'stale or invalid merge parents; request a fresh run after the merge ref is current'
printf 'merge=%s\n' "$merge" >> "${GITHUB_OUTPUT:?missing output path}"
echo 'PR merge preflight: current ordered pair verified; final current-pair verification still required'
