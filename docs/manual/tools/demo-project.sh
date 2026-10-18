#!/usr/bin/env bash
# Build a throwaway demo project for the manual screenshots.
# Usage: demo-project.sh /path/to/orch [sandbox-dir]
# Prints the project path on stdout; everything else goes to stderr.
# Only shell agents ever run. Other adapters are configured but never executed.
# WARNING: the finished project has agents on AI adapters (claude, codex, opencode, cursor).
# Never serve it, open it in the dashboard or run it with a real PATH or HOME.
set -euo pipefail
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

ORCH=${1:?usage: demo-project.sh /path/to/orch [sandbox-dir]}
[ -x "$ORCH" ] || { echo "not executable: $ORCH" >&2; exit 1; }
ORCH=$(cd "$(dirname "$ORCH")" && pwd)/$(basename "$ORCH")

REAL_HOME=$(cd "$HOME" && pwd -P)
if [ -n "${2:-}" ]; then
  # orch looks for .orchestry/ in parent directories, so a sandbox under the real home could hit a real project.
  [[ "$(cd "$(dirname "$2")" && pwd -P)/" != "$REAL_HOME"/* ]] || { echo "refusing a sandbox under the real HOME: $2" >&2; exit 1; }
  mkdir -p "$2"
  S=$(cd "$2" && pwd -P)
else
  S=$(cd "${TMPDIR:-/tmp}" && pwd -P)
  S=$(mktemp -d "$S/orch-manual-demo.XXXXXX")
fi
[[ "$S/" != "$REAL_HOME"/* ]] || { echo "refusing a sandbox under the real HOME: $S" >&2; exit 1; }
mkdir -p "$S/home" "$S/proj"

export HOME="$S/home"
export ORCHESTRY_GLOBAL_CONFIG_PATH="$S/home/global.yml"
export NO_COLOR=1
unset EDITOR VISUAL ORCH_WEB_PASSWORD
cd "$S/proj"

exec 3>&1 1>&2
o() { "$ORCH" "$@" --quiet; }

"$ORCH" init
[ -d "$PWD/.orchestry" ] || { echo "orch init did not create .orchestry in $PWD" >&2; exit 1; }
git init -q .
git -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m "demo"

# Safety: `orch run` dispatches work. It only ever runs while every agent in the project uses the
# shell adapter. The default claude-adapter agent from `orch init` is removed first, the non-shell
# display agents are added only after all runs have finished, and no orchestrator starts after that.
"$ORCH" agent remove agt_creator --quiet >/dev/null
adapters=$("$ORCH" agent list --json | node -e '
  const a = JSON.parse(require("fs").readFileSync(0, "utf8"));
  console.log((Array.isArray(a) ? a : a.agents).map((x) => x.adapter).join(" "))')
[ -z "$adapters" ] || { echo "expected an empty agent list before the runs, found: $adapters" >&2; exit 1; }

id_of() { "$ORCH" agent list --json | node -e '
  const n = process.argv[1];
  const a = JSON.parse(require("fs").readFileSync(0, "utf8"));
  const list = Array.isArray(a) ? a : a.agents;
  const hit = list.find((x) => x.name === n);
  if (!hit) process.exit(1);
  process.stdout.write(hit.id);' "$1"; }

only_shell() {
  "$ORCH" agent list --json | node -e '
    const a = JSON.parse(require("fs").readFileSync(0, "utf8"));
    const bad = (Array.isArray(a) ? a : a.agents).filter((x) => x.adapter !== "shell");
    if (bad.length) { console.error("refusing to run: non-shell agents present: " + bad.map((x) => x.name + "/" + x.adapter).join(", ")); process.exit(1); }'
}
run_task() { only_shell; "$ORCH" run "$1" --quiet >/dev/null; }

# Phase 1: shell agents only, then the real runs.
o agent add Builder  --adapter shell --command "echo compiling; sleep 1; echo build ok" --role "Build runner: compiles the project" --approval-policy auto
o agent add Tester   --adapter shell --command "echo running tests; sleep 1; echo 42 passed" --role "Test runner: runs the test suite" --approval-policy auto
o agent add Reviewer --adapter shell --command "echo checking diff; sleep 1; echo ready for human review" --role "Reviewer: prepares changes for human sign-off" --approval-policy suggest
o agent add Flaky    --adapter shell --command "echo connecting; sleep 1; echo 'error: upstream timed out after 30s calling https://lookup.example.invalid/v2/addresses (attempt 1 of 1). Check the service status page, then retry the task' >&2; exit 1" --role "Integration runner: calls an external service" --approval-policy auto
BUILDER=$(id_of Builder); TESTER=$(id_of Tester); REVIEWER=$(id_of Reviewer); FLAKY=$(id_of Flaky)

GOAL=$(o goal add "Ship signup validation" --description "Validate the signup form end to end before the 1.2 release")

T_BUILD=$(o task add "Build the signup service" -p 2 --assignee "$BUILDER" --max-attempts 1 --goal-id "$GOAL" -d "Compile the service and report the result")
T_TEST=$(o task add "Run the signup test suite" -p 2 --assignee "$TESTER" --max-attempts 1 --goal-id "$GOAL" -d "Run all signup tests")
T_REVIEW=$(o task add "Prepare the validation change for review" -p 1 --assignee "$REVIEWER" --max-attempts 1 --goal-id "$GOAL" -d "Check the diff and hand it to a human")
T_FLAKY=$(o task add "Call the address-lookup service" -p 3 --assignee "$FLAKY" --max-attempts 1 -d "Look up addresses through the external service")

run_task "$T_BUILD"
run_task "$T_TEST"
run_task "$T_REVIEW"
run_task "$T_FLAKY" || only_shell

"$ORCH" task list --json | node -e '
  const t = JSON.parse(require("fs").readFileSync(0, "utf8"));
  const list = Array.isArray(t) ? t : t.tasks;
  const count = (s) => list.filter((x) => x.status === s).length;
  const want = { in_progress: 0, review: 1, done: 2, failed: 1 };
  for (const [s, n] of Object.entries(want)) {
    if (count(s) !== n) { console.error(`demo: expected ${n} ${s} task(s), found ${count(s)}`); process.exit(1); }
  }'

# Phase 2: display-only agents, team, tasks and messages. No `orch run` below this line.
o agent add Ada      --adapter claude   --model claude-sonnet-4-6 --role "Lead engineer: designs changes and breaks down work" --approval-policy suggest
o agent add Grace    --adapter codex    --model gpt-5 --role "Backend engineer: APIs, storage and migrations" --approval-policy auto
o agent add Linus    --adapter opencode --role "Platform engineer: CI, builds and releases" --approval-policy auto
o agent add Margaret --adapter cursor   --role "Frontend engineer: forms, accessibility and styling" --approval-policy suggest
ADA=$(id_of Ada); GRACE=$(id_of Grace); LINUS=$(id_of Linus); MARGARET=$(id_of Margaret)

o goal update "$GOAL" --assignee "$ADA"
TEAM=$(o team create Platform --lead "$ADA" --members "$GRACE,$LINUS,$BUILDER,$TESTER" -d "Owns the signup service")

T1=$(o task add "Add input validation to the signup form" -p 1 --labels frontend,validation --goal-id "$GOAL" -d "Reject empty and malformed email addresses before submit")
o task add "Add a unique index on users.email" -p 2 --labels backend --goal-id "$GOAL" -d "Ship the migration with a rollback" >/dev/null
o task add "Cache dependencies in CI" -p 3 --labels ci >/dev/null
o task add "Write the 1.2 release notes" -p 4 --labels docs >/dev/null
o task add "Fix the password strength meter" -p 2 --depends-on "$T1" --labels frontend >/dev/null
o team add-task "$TEAM" "$T_TEST" >/dev/null

# Messages.
"$ORCH" msg send "$GRACE" "The unique index on users.email needs a migration. Please add a rollback script and keep the lock time short on the production table." --subject "Migration for users.email" --from "$ADA" --quiet
"$ORCH" msg send "$MARGARET" "Validation should run on blur and again on submit. Show the error text under the field and keep the focus on the first invalid field." --subject "Validation behaviour" --from "$ADA" --quiet
"$ORCH" msg broadcast "Release 1.2 freezes on Friday. Finish open work or tell me what slips." --subject "Release freeze" --from "$ADA" --quiet

cat > "$S/proj/WARNING.md" <<WARN
# Do not run this project

This is a throwaway demo project for screenshots. It contains agents on AI adapters:
Ada (claude), Grace (codex), Linus (opencode) and Margaret (cursor). Serving it, opening the
dashboard on it, or running tasks with a real PATH or HOME can start those real AI CLIs.
Delete the whole sandbox directory instead: $S
WARN
echo "WARNING: $S/proj has claude/codex/opencode/cursor agents: never serve or run it with a real PATH or HOME" >&2
echo "demo project ready: $S/proj" >&2
echo "$S/proj" >&3
