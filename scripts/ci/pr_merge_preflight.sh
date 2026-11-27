#!/usr/bin/env bash
# Resolve once per event, before classification/builds. Never reopen a PR or
# change its head. The outer deadline includes all lookups, fetches and waits.
set -euo pipefail

refuse() {
  echo "::error::PR merge preflight: $*" >&2
  exit 1
}

if [[ ${1:-} != --bounded ]]; then
  limit=${PR_MERGE_TIMEOUT_SECONDS:-120}
  [[ $limit =~ ^[1-9][0-9]*$ && $limit -le 120 ]] || refuse 'invalid resolution deadline'
  if timeout --signal=KILL "${limit}s" bash "$0" --bounded; then
    exit 0
  else
    status=$?
    refuse "resolution failed or total deadline expired (status=$status); no verified output"
  fi
fi

merge=$(git rev-parse --verify HEAD)
parents=$(git show -s --format=%P "$merge")
printf 'observed event merge=%s parents=[%s]\n' "$merge" "$parents"
echo 'expected base=<indeterminate> head=<indeterminate> (live lookup pending)'
[[ ${PR_NUMBER:-} =~ ^[1-9][0-9]*$ ]] || refuse 'invalid PR number'
[[ ${PR_HEAD_SHA:-} =~ ^[0-9a-f]{40}$ ]] || refuse 'invalid event head'
[[ ${GITHUB_REPOSITORY:-} =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || refuse 'invalid event repository'
base_ref="refs/heads/${PR_BASE_REF:?missing base ref}"
head_ref="refs/pull/$PR_NUMBER/head"
merge_ref="refs/pull/$PR_NUMBER/merge"
git check-ref-format "$base_ref" || refuse 'invalid base ref'
printf 'expected event repository=%s PR=%s target=%s head=%s\n' "$GITHUB_REPOSITORY" "$PR_NUMBER" "$PR_BASE_REF" "$PR_HEAD_SHA"
: "${GITHUB_OUTPUT:?missing output path}"

live_pair() {
  # Public Git reads remain anonymous; only this API request gets a read token.
  local identity refs sha ref extra
  local -a auth=()
  if [[ -n ${GH_TOKEN:-} ]]; then auth=(-H "Authorization: Bearer $GH_TOKEN"); fi
  identity=$(curl --fail --silent --show-error --max-time 20 \
    "${auth[@]}" -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$GITHUB_REPOSITORY/pulls/$PR_NUMBER") || refuse 'live PR lookup failed'
  # Parse JSON structurally. Event identity, target and head are immutable for
  # this run; base-tip motion alone may be resolved within the deadline.
  if ! python3 -c '
import json, os, sys
p = json.load(sys.stdin)
print("observed PR " + json.dumps(dict(number=p["number"], state=p["state"],
    repository=p["base"]["repo"]["full_name"], target=p["base"]["ref"],
    head=p["head"]["sha"]), sort_keys=True))
valid = (p["number"] == int(os.environ["PR_NUMBER"])
         and p["state"] == "open"
         and p["base"]["repo"]["full_name"] == os.environ["GITHUB_REPOSITORY"]
         and p["base"]["ref"] == os.environ["PR_BASE_REF"]
         and p["head"]["sha"] == os.environ["PR_HEAD_SHA"])
if not valid:
    sys.exit("live PR metadata differs from event")
' <<< "$identity"; then
    refuse 'live PR identity, target or head differs from event; fresh event required'
  fi
  refs=$(GIT_TERMINAL_PROMPT=0 git ls-remote --refs origin "$base_ref" "$head_ref" "$merge_ref") || refuse 'live ref lookup failed; expected pair indeterminate'
  expected_base= expected_head= advertised_merge=
  while read -r sha ref extra; do
    [[ $sha =~ ^[0-9a-f]{40}$ && -z $extra ]] || refuse 'malformed live ref'
    case "$ref" in
      "$base_ref")
        [[ -z $expected_base ]] || refuse 'duplicate live base'
        expected_base=$sha ;;
      "$head_ref")
        [[ -z $expected_head ]] || refuse 'duplicate live head'
        expected_head=$sha ;;
      "$merge_ref")
        [[ -z $advertised_merge ]] || refuse 'duplicate live merge'
        advertised_merge=$sha ;;
      *) refuse 'unexpected live ref' ;;
    esac
  done <<< "$refs"
  printf 'expected base=%s head=%s advertised_merge=%s\n' "${expected_base:-<indeterminate>}" "${expected_head:-<indeterminate>}" "${advertised_merge:-<unpublished>}"
  [[ -n $expected_base && -n $expected_head ]] || refuse 'live ref missing; expected pair indeterminate'
  [[ $expected_head == "$PR_HEAD_SHA" ]] || refuse 'live head differs from event; fresh event required'
}

live_pair
while :; do
  if [[ $parents == "$expected_base $expected_head" ]]; then
    verified_base=$expected_base
    # Also re-read identity and refs on the event fast path. Never publish a
    # candidate whose base/head pair changed while it was being fetched.
    live_pair
    if [[ $parents == "$expected_base $expected_head" ]]; then
      printf 'verified merge=%s tree=%s base=%s head=%s\n' "$merge" "$(git rev-parse "$merge^{tree}")" "$verified_base" "$expected_head"
      printf 'merge=%s\nbase=%s\n' "$merge" "$verified_base" >> "$GITHUB_OUTPUT"
      echo 'PR merge preflight: current ordered pair verified; final current-pair/tree qualification still required'
      exit 0
    fi
  fi
  if [[ -n $advertised_merge ]]; then
    # Fetch the advertised object by SHA, never by the moving ref. A changed
    # advertisement cannot silently select a different object during fetch.
    GIT_TERMINAL_PROMPT=0 git fetch --no-tags --depth=2 origin "$advertised_merge" || refuse 'candidate fetch failed'
    merge=$(git rev-parse --verify "$advertised_merge^{commit}")
    parents=$(git show -s --format=%P "$merge")
    printf 'observed candidate merge=%s parents=[%s]\n' "$merge" "$parents"
    live_pair
    if [[ $parents == "$expected_base $expected_head" ]]; then continue; fi
  fi
  echo 'PR merge preflight: awaiting publication of current ordered pair'
  sleep 1
  live_pair
done
