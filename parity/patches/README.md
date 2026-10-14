# Oracle patches

The parity oracle is the TypeScript ORCH at pinned commit `c066dc0` (v1.0.34).
It is the **reference**, so patching it is close to the last thing we want to
do: every patch here moves the thing we are measuring against.

Each patch therefore needs all four of:

1. a defect that stops a documented behaviour from being observable at all, so
   the affected scenarios cannot be differential either way;
2. the **smallest** change that restores the behaviour the source itself
   already documents as intended — no refactoring, no drive-by fixes;
3. a reproduction in this file, and the matrix of what does and does not work;
4. a `§14` entry recording the comparison effect as `oracle patched`.

`parity/setup.sh` applies every `*.patch` here after checkout and before the
build, and writes `parity/orch-ts/.parity-stamp`. **`cargo xtask parity`
refuses to run** if the stamp is missing, does not list every patch, or does
not match the built `dist/` — so an unpatched or stale oracle cannot quietly
grade r-orch.

To re-apply by hand: `./parity/setup.sh`.

---

## 0001 — keep the event loop alive in `spawnAndWait`

**Effect without the patch: `run <id>` and `serve --once` exit 0 without
dispatching anything, whenever the workspace mode is `worktree` — the
default.** Ten scenarios covering run/serve orchestration cannot be
differential at all against an oracle that never dispatches.

### The defect

`ProcessManager.spawn` ends with `proc.unref()`, so a long-lived agent process
cannot hold the CLI open. Its own comment records the intended escape hatch:

```js
// Allow parent to exit without waiting for this child.
// Pipes (stdout/stderr) still hold refs while being read — that's intentional.
proc.unref();
```

That is what keeps the other two call sites alive: `spawnAndCapture` attaches a
`data` handler, and `MergeStrategy.mergeBack` attaches handlers to both pipes.

`WorkspaceManager.spawnAndWait` reads **neither** pipe. It awaits a promise that
settles on `'close'`, and with no ref'd handle left in the loop, node exits
before the `'close'` event is delivered — so the promise never settles and the
`await` never returns. `prepareWorktree` awaits `git worktree add` through this
method, which is why only worktree mode is affected.

The patch calls `proc.ref()` for the duration of the wait, which is what
"spawn **and wait**" already promises. One line plus a comment.

### Reproduction

Measured on the pinned oracle, each in a fresh `git init`'d tmpdir with a
shell agent and one assigned task:

| `defaults.agent.workspace_mode` | command | exit | task status | runs written |
|---|---|---|---|---|
| `worktree` (default) | `run <id>` | 0 | **todo** | **0** |
| `worktree` (default) | `serve --once` | 0 | **todo** | **0** |
| `shared` | `run <id>` | 0 | done | 2 |
| `shared` | `serve --once` | 0 | done | 2 |

The giveaway is that `git worktree list` shows **two** worktrees afterwards: the
`git worktree add` did its work, and node exited before being told it had
finished. Nothing errors, and the exit code is 0, so a scenario that only
checked exit codes would call this a pass.

With the patch, all four rows reach `done` with 2 runs.

One wrinkle worth knowing when reproducing: a *second* invocation in the same
project can appear to work, because the worktree from the first attempt already
exists and the reuse path takes different branches. Always reproduce from a
clean project directory.

### What is deliberately *not* patched

`MergeStrategy.mergeBack`'s inner `git merge --abort` has the same shape — it
attaches no pipe reader and awaits `'close'` — so it can hang or no-op on the
merge-conflict path. It is left alone: the conflict path is not on any current
scenario, and widening this patch past the one defect that blocks orchestration
would make the oracle less of a reference. Recorded here so it is a known
limitation rather than a surprise.
