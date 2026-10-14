# Gate security review — r-orch

**Reviewer:** security gate (read-only)
**Candidate:** worktree `../r-orch-wt/gate-review` @ `0b67e4d`
**Oracle:** `parity/orch-ts` (ORCH `c066dc0`, npm `@oxgeneral/orch` v1.0.34)
**Stance:** 0.5 — the stance never lowers the security floor
**Date:** 2026-10-14

---

## Verdict

> **PASS WITH CONDITIONS** — 6 conditions, all ship-blocking. Not a block: nothing found is
> code execution, privilege escalation, data exfiltration, or an unrecoverable state. But
> **C0 is a measured availability regression against the oracle on the primary config path and
> the gate does not pass until it is fixed.**

The posture on what this team controls is genuinely strong, and that is why this is a
conditional pass rather than a block:

- `unsafe_code = "forbid"` in **all 7** workspace members (`forbid` cannot be `#[allow]`-overridden,
  so this is a compiler guarantee, not a convention) — and **zero** first-party `unsafe` blocks,
  functions or impls. The only four `unsafe` grep hits in `crates/` and `xtask/` are comments
  explaining why it is avoided.
- **Zero** first-party `build.rs`, **zero** `[patch]` sections, **zero** git/branch/rev/tag
  dependencies, and no path dependency pointing outside the workspace.
- **Zero** network or TLS crates anywhere in the 336-entry lock file. This *structurally* proves
  the §13 "no network, no self-update" decision instead of resting on the string assertion in
  `crates/orch-cli/src/update.rs:95`. This is the strongest single piece of evidence in the review.
- Two places where r-orch is **materially safer than the original** (§5).

Residual risk is concentrated in two *transitive* subtrees — `unsafe-libyaml` and the
`arboard`→image-decoder stack — not in the ported code.

---

## 1. Conditions (ship-blocking)

### C0 — HIGH · YAML alias amplification DoS · CWE-776/CWE-400 · **new regression vs the oracle**

**Location:** `crates/orch-infra/src/storage/fs_utils.rs:215` (`read_yaml_value` → `serde_yaml_ng::from_str`)

`serde_yaml_ng` materialises every alias as a **deep copy** (its `de.rs` `jump()` re-walks the
anchored event range per use). `js-yaml` returns the **same reference**, so it is O(input). The
only guard in `serde_yaml_ng` is `jumpcount > events.len() * 100`, which bounds the *number* of
jumps but not the *work per jump* — so a single large flat anchor aliased many times costs one
`jumpcount` per alias and a full subtree copy per alias. The result is quadratic, and the
repetition limit never fires.

This is reached by **every** YAML read: `config.yml`, agents, tasks, goals, teams, global config,
`_index.json`. `config.yml` is read by effectively every `orch` invocation.

**Measured.** There are **two distinct amplifying shapes**, found independently and reconciled.
Both were timed end-to-end through the real binary (`orch status`) against `js-yaml` 4 from
`parity/orch-ts/node_modules` on byte-identical input. The cost model is
`≈ (jump budget) × (size of the anchor's subtree per jump)`, and both factors grow with input —
hence superlinear.

*Shape A — nested aliases over a large **sequence** leaf* (depth 6, plain aliases, no merge keys).
The more efficient attack by a wide margin:

| config.yml bytes | js-yaml | r-orch (debug) | ratio | exit |
|---|---|---|---|---|
| 9,966 | 6 ms | 20.42 s | 3,400× | error |
| 39,666 | 10 ms | **5 min 13 s** | **31,000×** | error |

*Shape B — one large flat anchor aliased many times*. Less efficient per byte, but **exits 0**:

| config.yml bytes | js-yaml | r-orch (release) | r-orch (debug) | ratio (release) | exit |
|---|---|---|---|---|---|
| 54,301 | 11.8 ms | 1.39 s | 2.74 s | 118× | 0 |
| 106,501 | 16.2 ms | 4.12 s | 8.11 s | 254× | 0 |
| 210,901 | 26.3 ms | 12.64 s | 25.48 s | 481× | 0 |
| 322,901 | 25.3 ms | 35.16 s | — | 1,390× | 0 |

js-yaml is linear in file size in both shapes; r-orch is superlinear in both (Shape A: 4× input →
15× time). **A ~40 KB `config.yml` is enough to cost 5 minutes of pinned CPU per `orch`
invocation** — small enough to pass casual review in a PR diff.

Two properties make this worse than a plain hard failure:

- **Shape B exits 0** — the parse *succeeds*. No error, no repetition-limit rejection, no crash,
  just a wedge.
- **Shape A's error is not a circuit breaker.** It does end in
  `repetition limit exceeded`, but only *after* running the full 5 minutes: `serde_yaml_ng`'s
  limit caps the jump **count**, not the deep-copy work each jump performs.

**Reproducers.** Shape A (the one to test a fix against — the large *sequence* leaf is the
required amplifier; an identical bomb with a *scalar* leaf is harmless, which is the reconciliation
described in §7):

```yaml
a0: &a0 ["xxxxxxxxxx", ... W times]   # leaf MUST be a collection, not a scalar
a1: &a1 [*a0, ... W times]
a2: &a2 [*a1, ... W times]
a3: &a3 [*a2, ... W times]
a4: &a4 [*a3, ... W times]
a5: &a5 [*a4, ... W times]
```

`W = 1200` yields 39,666 bytes. Shape B:

```python
SIZE, N = 60000, 2000
body = ", ".join(["'x'"] * SIZE)
doc = "\n".join([f"big: &big [{body}]"] + [f"k{i}: *big" for i in range(N)]) + "\n"
open(".orchestry/config.yml", "w").write(doc)
```

**Exploit scenario.** Adapters are invoked with `--dangerously-skip-permissions` /
`bypassPermissions` / `--yolo --trust` (parity-required) and run with write access to the project
directory, so **an agent CLI can plant this file itself**. A malicious PR adding a
`.orchestry/config.yml` does it too. Every subsequent `orch` invocation — including `orch status`,
which a user runs to find out what is wrong — hangs for the full parse. The TS original does not
have this: js-yaml handles the same file in 25 ms.

**Remediation.** Note what does *not* work: a size cap alone is useless (10 KB already costs
20 s), and an alias **count** cap alone is insufficient because cost is
`count × subtree size` — 64 aliases over a 1 MB anchor is still tens of seconds. Do all three:

1. **Pre-scan the raw text** and reject a document that exceeds a sane size (~1 MB), **or**
   carries more than ~64 `*alias` tokens, **or** contains *alias nesting* — an alias appearing
   inside an anchored collection that is itself aliased. The third is the actual amplifier and is
   cheap to detect in a single scan.
2. **Run `from_str` in `spawn_blocking` under a hard wall-clock timeout**, so a pathological
   document fails fast instead of pinning a core for minutes. This is the robust backstop and the
   only mitigation that does not depend on predicting the attack shape.
3. **Do not rely on `serde_yaml_ng`'s repetition limit.** It fires only after the work is done.

Verify any fix against **Shape A at W = 1200**, not against a scalar-leaf bomb.

Record as a `§14` entry with its `Effect on differential comparison` field: the oracle accepts
documents r-orch would now reject, so this needs `scenario exclusion required` or an explicit note.

Worth evaluating together with C5: `serde_norway` (the pre-vetted escape hatch already named in
the root `Cargo.toml`) reportedly uses the safe-Rust `libyaml-safer`, and **may** have different
alias semantics. Verify by measurement against the table above before assuming it fixes C0.

---

### C1 — MEDIUM · unguarded pid cast in the lock · CWE-190/CWE-754 · **divergence**

**Location:** `crates/orch-infra/src/storage/lock.rs:210`

```rust
match kill(Pid::from_raw(pid as i32), None) {
```

This is the one pid path that skips the guard the project applies everywhere else. `pid as i32`
wraps silently, so a lock file containing `4294967295` becomes `kill(-1, 0)` → `Ok` → reported
**alive**; `4294967295 − N` probes process **group** N. Only signal `0` is used, so nothing is
delivered — the impact is a liveness misreport, not a stray signal.

**Not parity.** Node's `process.kill` validates pid as int32 and throws `ERR_OUT_OF_RANGE`, which
`isProcessAlive`'s catch turns into `false`. So TS clears such a lock and r-orch wedges it
(`acquire_lock` refuses, exit 4) until the 60 s mtime staleness window elapses — indefinitely if
anything keeps the file touched.

Conversely r-orch is **safer** than TS on negative pids: `parse::<u32>()` at `lock.rs:186` rejects
`-1`, where TS `parseInt` accepts it and runs `process.kill(-1, 0)` → "alive". The fix keeps that
improvement.

**Remediation.** Reuse `signal_target` from `crates/orch-infra/src/process/manager.rs:55`
(`i32::try_from` + `> 0`) inside `is_process_alive`. Treating `0` as dead is itself an L1
divergence from TS — record it as a `§14` row, as was done for dissent item #16.

*Found independently by me and by the delegated parsing audit, which measured it.*

---

### C2 — MEDIUM · unbounded run-event read · CWE-400/CWE-789 · **new regression vs the oracle**

**Location:** `crates/orch-infra/src/storage/run.rs:184` (`read_events_file`)

```rust
let content = tokio::fs::read(path).await  // whole file, no size guard
```

Then every `RunEvent` is collected into a `Vec`. This bypasses the `MAX_JSONL_READ_SIZE` = 50 MB
guard and tail fallback that `crates/orch-infra/src/storage/fs_utils.rs:652` applies to every
*other* JSONL read.

**Verified against the oracle myself:** `parity/orch-ts/src/infrastructure/storage/run-store.ts:79`
uses `createReadStream` + `readLines`, i.e. constant memory. A chatty or long-running agent grows
`.orchestry/runs/<id>.jsonl` without bound; `orch logs [--follow]` and the TUI disk-observer
fallback then OOM.

**Remediation.** Stream the file with the same line semantics, or at minimum apply the existing
50 MB guard + tail fallback so the two JSONL paths agree.

---

### C3 — LOW · `..` reaches two extension-less path builders · CWE-22 · parity

**Locations:** `crates/orch-infra/src/storage/paths.rs:166` (`task_attachments_dir`),
`crates/orch-infra/src/workspace/manager.rs:49` (`workspace_path`)

`sanitize_id` correctly accepts `..` — `.` is in the reference character class `^[A-Za-z0-9._-]+$`,
and `paths.rs:290` tests and documents this deliberately rather than silently diverging. It is
harmless in every builder that appends an extension (`..` + `.yml` is the ordinary filename
`...yml`). Two builders append nothing:

- `task_attachments_dir("..")` → `.orchestry/attachments/..`
- `workspace_path("..")` → `.orchestry/workspaces/..` = **`.orchestry`**, which `cleanup` then
  hands to `tokio::fs::remove_dir_all` (`manager.rs:200`)

**Identical in the oracle** — `parity/orch-ts/src/infrastructure/workspace/workspace-manager.ts:76`
is the same `path.join(orchestryDir, 'workspaces', sanitizeId(taskId))` followed by
`fs.rm(..., {recursive: true, force: true})`. Not a new risk, and it needs a hand-edited task
file because ids are nanoid-generated. The destructive sink is what makes it worth closing anyway.

**Remediation.** Reject exactly `.` and `..` in those two builders only. Purely additive — real
ids never contain them — so `sanitize_id` itself stays byte-compatible and no on-disk or CLI
contract changes.

---

### C4 — LOW · unchecked token-sum overflow · CWE-190 · **new crash vector**

**Location:** `crates/orch-domain/src/run.rs:108`

```rust
total: input + output + reasoning,
```

`number_field` (`crates/orch-infra/src/adapters/util.rs:67`) returns a child CLI's JSON number as
`u64` verbatim, up to `u64::MAX`. There is no `overflow-checks` override in `Cargo.toml`, so the
debug/test profile has overflow checks **on**: a child emitting two `u64::MAX` counts **panics**;
release wraps silently. TS produces a float. Untrusted child output reaching a panic.

**Remediation.** `saturating_add`.

---

### C5 — PROCESS · no advisory scanner exists, and an archived YAML parser is on the untrusted path

**(a) The vulnerability-scanning control is absent, not merely unrun.** `cargo-audit`,
`cargo-deny` and `cargo-outdated` are all missing from the machine (`cargo audit --version` →
`no such command: audit`), and there is no `deny.toml`, `audit.toml` or `.github/` in the tree.
Nothing was installed for this review. **This gate therefore cannot assert "no known
vulnerabilities"** — neither my review nor the delegated dependency audit is a substitute for a
live RustSec query, and both carry a May 2026 knowledge cutoff. Add a `cargo-deny` config plus a
CI advisory job and re-run.

**(b) `serde_yaml_ng 0.10.0` → `unsafe-libyaml 0.2.11`**, reached from `orch-infra` and `xtask`.
`unsafe-libyaml` is **archived upstream** — a mechanical C-to-Rust transpile of libyaml, a large
body of machine-generated raw-pointer `unsafe` receiving no security maintenance — and it is the
parser for untrusted YAML. It is the largest `unsafe` surface in the compiled graph, and the
`serde_yaml_ng` fork around it is single-maintainer. The root `Cargo.toml` already names the
escape hatch. **Either take `serde_norway` or record an explicit, signed-off risk acceptance.**
Evaluate jointly with C0.

---

## 2. Recommended, not conditions

| # | Severity | Item |
|---|---|---|
| R1 | MEDIUM | **Agent API keys are world-readable and un-ignored.** `AgentConfig.env` (`crates/orch-domain/src/agent.rs:153`) is where users put API keys; it persists to `.orchestry/agents/*.yml`. There is no `set_permissions`/`mode(` call anywhere in `crates/` or `xtask/`, so files land at 0644 and dirs at 0755. Compounding it, `crates/orch-cli/src/commands/init.rs:35` writes a `.gitignore` covering `state.json`, `*.lock`, `runs/`, `logs/`, `workspaces/` — but **not** `agents/` or `config.yml`. Any local user reads the keys; the next commit publishes them. Byte-identical in the oracle (`init.ts:60`), so this is parity — but the `chmod` half (0600 on `config.yml` + `agents/*.yml`, 0700 on `.orchestry/`) changes no format and is free. Adding `agents/` to the ignore list changes an L0 file and needs a `§14` decision. |
| R2 | LOW | **Temp file in `atomic_write` lacks `O_EXCL`.** `crates/orch-infra/src/storage/fs_utils.rs:41` uses `tokio::fs::write` on `.{basename}.{8hex}.tmp` — no `O_EXCL`, no `O_NOFOLLOW` — so a symlink pre-placed at that exact name redirects the content. `random_hex8` is 32 real CSPRNG bits and only the naming *shape* is contract, so `OpenOptions::new().create_new(true).write(true)` closes it at zero parity cost. The destination is already safe: `rename(2)` replaces the symlink inode and never follows it. |
| R3 | LOW | **JSONL append and `--log-file` follow symlinks.** `fs_utils.rs:613` and `crates/orch-cli/src/serve/mod.rs:46` both `create(true).append(true).open(path)` with no `O_NOFOLLOW`. Both are parity; `--log-file` is the user's own flag (equivalent to shell redirection) and crosses no privilege boundary. At `--verbose` the log carries `agent:output`. |
| R4 | MEDIUM (surface) | **`arboard 3.6.1` pulls a full image-decoder stack** — `image 0.25.10` → `tiff` → `zune-jpeg`, plus `png`, `fax`, `weezl`, `moxcms`, `pxfm`, `x11rb`, `objc2-*` — so a terminal orchestrator can read clipboard *images*. Decoders are historically advisory-dense and clipboard content is attacker-influencable. No specific known vuln; this is excess attack surface. Disabling the image feature drops ~a dozen parsers. The mechanism is still an improvement on TS, which shells out to `osascript`/`xclip`. |
| R5 | INFO | **`tracing` is never subscribed.** `tracing-subscriber` is declared (`Cargo.toml:85`) and never referenced — no `tracing_subscriber::`, `set_global_default`, `init()` or `try_init` in the tree. Every `tracing::` call is a no-op, so ARCHITECTURE §4's "must not write stdout" holds *vacuously* and the documented `ORCHESTRY_DEBUG`/`RUST_LOG` → stderr path is unimplemented. Foot-gun: the obvious fix, `tracing_subscriber::fmt::init()`, defaults to **stdout** and would break the L0 CLI contract while starting to emit `workspace/manager.rs:58`'s `?args`. Install it now, pinned to `.with_writer(std::io::stderr)`, with a test asserting stdout stays empty at `RUST_LOG=trace`. |
| R6 | INFO | **`kstring` is pinned backwards** to 2.0.2 while `liquid 0.26.11` selects 2.0.5 (whose MSRV 1.96.0 exceeds the pinned toolchain 1.94.1). No known advisory against any 2.0.x, so present risk is low, but it is a standing blocker on patch-level security updates in the liquid tree, removable only with a toolchain bump. Correctly commented in-manifest; track as debt. |
| R7 | INFO | **`fancy-regex 0.11.0` backtracks and is ~3 years stale** (`crates/orch-infra/src/models/parsers.rs:17`). Unlike `regex`, it carries catastrophic-backtracking potential; the pattern is a lazy star with lookahead over subprocess `--help` output (low-trust but not normally attacker-controlled) and the crate enforces a default backtrack limit. Net low. |
| R8 | INFO | **`tui-textarea-2 0.13.2` is a young single-maintainer fork** in the keyboard-input path (upstream `tui-textarea` and `tui-input` are both absent from the lock). Adoption rationale is sound — upstream hard-pins ratatui 0.29 — but pin it exactly (`=0.13.2`) and review on upgrade rather than floating. |
| R9 | INFO | **`nanoid 0.5.0` is effectively unmaintained** (last release 2022). Tiny, and the RNG sourcing is sound (`rand 0.9.5` → `getrandom 0.3.4`, OS CSPRNG). Ledger item; replacement is ~20 lines. |

---

## 3. Cleared after investigation

- **42-bit nanoid ids are not a weakness here.** `ID_LENGTH = 7` over a 64-char alphabet = 2^42,
  which would matter if an id were ever an unguessable capability. It is not: there is no
  authn/authz layer in the product and **no network surface in the entire dependency graph**, so
  ids are purely local filesystem handles and nothing depends on their unguessability. Worth
  stating explicitly in PARITY §6.2 so a future reader does not promote one to a token.
- **`random_hex8`'s 32 bits are correctly scoped.** It is a collision nonce for a temp file
  created *beside* its destination in the user's own tree, not in shared `/tmp` — same scheme and
  entropy as the oracle's `randomBytes(4)`. Not a predictable-temp-name vulnerability. (See R2 for
  the separate, real `O_EXCL` gap.)
- **`process-wrap` is moot.** Declared in `[workspace.dependencies]` but **absent from
  `Cargo.lock`** — no member depends on it, so it is never resolved or compiled. Zero exposure.
  Same for `tui-input`, `proptest` and `serde_norway`.
- **`attachment_path` is production-unreachable.** `crates/orch-engine/src/task_service.rs:332`
  joins an unsanitized `filename`, but every caller is a test. It is `pub` library API and shaped
  exactly like the oracle's `getAttachmentPath`, so it is a latent footgun rather than a finding.
- **Duplicate major versions are not a security finding.** 13 crates at multiple versions; the
  ~120-crate gap between the 336 lock entries and the 203 actually compiled is the
  `termwiz`/`wezterm-*` subtree behind ratatui's *unenabled* optional backends — lock-resident,
  never built. All 336 resolve from crates.io.

---

## 4. Parity-required risks — documented, and r-orch is not worse

Judged per the brief: does r-orch make these *worse* than the original, and is a fix possible
without breaking the on-disk/CLI contract?

| Behaviour | Verdict |
|---|---|
| `--dangerously-skip-permissions` / `--permission-mode bypassPermissions` / `--yolo --trust` passed to agent CLIs | Identical to the oracle. Not fixable without breaking the L0 adapter-argv contract (PARITY §1.1.7). This is the premise of the product, and it is the threat model that makes C0 reachable. |
| `bash -lc <agent.config.command>` for the shell adapter | Identical. `config.command` is the agent's own configuration, as trusted as the orchestrator. Prompt goes via `ORCHESTRY_TASK_PROMPT`, **not** interpolated into the command string — correct. Note `-l` sources login profiles. |
| Adapters inherit the full process environment (no `env_clear`) | Identical. Leaks any secret env to agents, as TS does; also what makes `CLAUDECODE=1` work. |
| `npm test` / `npx tsc --noEmit` / `npm run lint` in the workspace cwd | Identical (§14.5). Executes the reviewed repo's own `package.json` scripts by design. Commands come from a **closed enum** mapped to `&'static str` (`review_runner.rs:38`) — no user string reaches argv. |
| `$EDITOR` split on whitespace and spawned | Identical to `editor.split(/\s+/)`. No shell. PATH resolution as in TS. |
| Unbounded `pending` buffer in the line reader | `crates/orch-infra/src/process/line_reader.rs:89` grows until a `\n`; the 16 KB cap is applied in `decode`, so it bounds the emitted line, **not** memory. I read the oracle's `readLines` (`process-manager.ts:104`) side by side — identically shaped. Exact parity, and the actor is an agent CLI already fully trusted. A hard abort past ~64 MB pending would be a cheap improvement in both. |

---

## 5. Where r-orch is materially **safer** than the original

Worth recording in `§14` — these are wins, not divergences to apologise for.

1. **Liquid templates cannot read the filesystem.** `crates/orch-infra/src/template/engine.rs:31`
   is `ParserBuilder::with_stdlib().build()` with **no partials source**, so `{% include %}`,
   `{% render %}` and `../` traversal all fail with `Partial does not exist`. The oracle's
   liquidjs defaults to `root: ['.']` with an fs loader, and was measured rendering **real file
   contents** (`package.json`, `tsconfig.json`, `src/index.ts`) into agent prompts. Since
   `config.prompt.*` templates are user- (and agent-) supplied, this closes a real file-disclosure
   path that the original has.
2. **Negative pids in the lock file are rejected.** `lock.rs:186` `parse::<u32>()` refuses `-1`,
   where the oracle's `parseInt` accepts it and probes every process the user can signal. (C1 is
   the other half of this same cast.)
3. **Clipboard uses `arboard` in-process** instead of shelling out to `osascript`/`xclip`/
   PowerShell — one fewer subprocess surface (at the cost of R4's decoder stack).

---

## 6. Checked clean

Per the collaboration protocol: what I examined and found nothing wrong with.

**Path construction** — every builder in `crates/orch-infra/src/storage/paths.rs` routes through
`sanitize_id`; the `..` case is explicitly tested at `paths.rs:290` rather than overlooked.
`validate_workspace_path` (`paths.rs:180`) does lexical containment without touching the
filesystem, correctly matching `path.resolve`. Sibling-prefix escape (`/tmp/project-other` vs
`/tmp/project`) is tested and rejected.

**Skill loader** — `is_valid_skill_name` (`crates/orch-core/src/skills.rs:24`) is `[a-z0-9-]+`:
no `.`, no `/`, stricter than `sanitize_id`. It is evaluated **before** anything touches the
filesystem, and the cache cannot be poisoned by an invalid name because entries are only inserted
post-validation. Traversal test present.

**Argument injection into git** — branch names are always `orchestry/<sanitized id>/<slug>` with
slug restricted to `[a-z0-9-]` and length-capped (`workspace/manager.rs:295`), so no value can
lead with `-`. `git`, `rsync`, `npm`, `npx` and model-discovery probes all receive argv **vectors**,
never a shell string.

**No shell interpolation anywhere else** — the only `sh`/`bash -c` outside `#[cfg(test)]` is the
documented shell adapter. `shell-words` is not used at all, as ARCHITECTURE §4.5 requires.

**Subprocess spawning** — `crates/orch-infra/src/process/manager.rs:131` sets
`process_group(0)` (safe std API, no `unsafe`), pipes all three streams, reaps in the background
so `is_alive` stays honest, and deliberately omits `kill_on_drop` to match node's `proc.unref()`.

**Pid guards in the ProcessManager** — `manager.rs:41-62` `signal_target` rejects `0` and
`> i32::MAX` with an accurate comment naming both hazards (`u32::MAX` → `-1` → every process).
This is the correct implementation; C1 is only about the one site that does not use it.

**YAML merge keys** — `apply_merge()` is unconditional at the single parse site
(`fs_utils.rs:220`), and all 8 callers (`agent.rs:36`, `task.rs:68`, `goal.rs:57`, `team.rs:35`/`51`,
`config.rs:36`, `global_config.rs:99`, `index.rs:113`, `init.rs:468`) route through it. No bypass.

**JSON depth** — serde_json's 128-frame recursion limit is enforced (measured to depth 100,000:
clean error, no stack overflow). No `disable_recursion_limit`, `unbounded_depth` or `from_reader`
anywhere. `serde_yaml_ng`'s `remaining_depth: 128` likewise errors cleanly on nested YAML — note
this is *depth*, which is why it does not help against C0's flat-wide shape.

**JSONL tail read** — `read_jsonl_tail` offset math uses `saturating_sub` and
`min(chunk, size - position)` under a `position <= size` invariant, at most 4 chunks (≤512 KB).
No overflow, no unbounded read. Corrupt lines are skipped with a 200-char preview.

**Line reader semantics** — 16 KB cap counted in UTF-16 code units, truncating rather than
erroring; empty-line drop, `\r` retention and trailing-fragment yield all match the oracle;
invalid UTF-8 → U+FFFD per line; chunk-split multibyte sequences reassembled before decode.
`EventBuffer` is bounded at 1024 with real backpressure (`push` waits), and the cap chain
16 KB → 8 KB → 4 KB is char-boundary safe.

**Lock file** — created with `create_new(true)` = `O_CREAT|O_EXCL`, which also refuses a
pre-placed symlink; garbage, negative and non-UTF-8 content all rejected (stricter than the
oracle's `parseInt`); `EPERM` correctly treated as alive; `touch_lock` writes zero bytes.

**Atomic write destination** — `fs_utils.rs:52` is `tokio::fs::rename`, and `rename(2)` replaces
the symlink inode rather than following it. Nothing opens the final path first. Pre-placing
`.orchestry/tasks/x.yml` as a symlink to `~/.ssh/authorized_keys` replaces the link and leaves
the target untouched. (R2 covers the separate temp-file gap.)

**Secrets** — **no `env::vars()`/`vars_os()` enumeration exists anywhere in the tree**, so no
environment snapshot can reach run JSONL, serve logs or stdout. `RunEvent`
(`crates/orch-domain/src/run.rs:173`) has no env or argv field. `SpawnOptions` and `ExecuteParams`
derive `Debug` over `env` but are never `Debug`-formatted. The tree's only `{:?}` print is
`eprintln!("{err:?}")` under `ORCHESTRY_DEBUG` (`crates/orch-cli/src/lib.rs:126`) on `Error`,
whose variants hold only context/path/command/status/output. The sole `?args` log is git argv.
`~/.orchestry/global.yml` holds UI preferences only; the lock file holds a PID only.

**Temp directories are 0700** — `tempfile` 3.27 `mkdtemp` for the `$EDITOR` buffer
(`crates/orch-cli/src/editor.rs:188`) and clipboard paste, so the 0644 files inside are not
other-readable. The oracle uses `mkdtemp` too.

**Dependency hygiene** — the classic offenders are all **absent**: `atty`, `ansi_term`, `instant`,
`term_size`, `users`, `stdweb`, `yaml-rust`, and any `time` 0.1/0.2 (only 0.3.55, well past the
`localtime_r` class). `nix 0.31.3`, `tempfile 3.27`, `which 8.0.6`, `dirs 7.0`, `jiff 0.2.38`,
`ratatui 0.30.2`/`crossterm 0.29` are all current, and the single-ratatui/single-crossterm claim
holds. This is a cleaner-than-average lock file. (Subject to C5(a): unscanned.)

---

## 7. Methodology, scope and a correction to my own first pass

**What I did.** Direct source review of the path-construction, subprocess, parsing, signalling and
secrets surfaces; targeted comparison against `parity/orch-ts` for every finding where parity was
the question; dependency and `unsafe` inspection of `Cargo.lock` and all 7 manifests; and the C0
measurements above. I delegated four parallel audits (dependencies/`unsafe`; untrusted-input
parsing; secrets/permissions/write-safety) and verified each of their load-bearing claims myself
before adopting it.

**A correction worth recording, because the process matters as much as the result.** My first
reproduction of C0 **failed** and I challenged the finding on that basis. I built nested alias
bombs at depth 6–14 and width 60–300 and measured 0.04–0.34 s with a clean
`Serde(Error("repetition limit exceeded"))` — linear and harmless.

The variable I had wrong was the **leaf anchor**. Mine was a scalar (`a0: &a0 'x'`), so each jump
deep-copied one scalar — linear, exactly my numbers. The original used a 300–1200 element
*sequence* leaf, so each jump deep-copied the whole collection. A controlled A/B at identical
depth and width isolates it cleanly:

```
leaf=scalar width=300   bytes=6,067    28 ms      ERR repetition limit exceeded
leaf=seq    width=300   bytes=9,966     7.1 s     ERR repetition limit exceeded
leaf=scalar width=1200  bytes=24,067   63 ms      ERR repetition limit exceeded
leaf=seq    width=1200  bytes=39,666  103.4 s     ERR repetition limit exceeded
```

Same code path, same terminating error, 250–1,600× apart on leaf shape alone — and the scalar
column reproduces my own measurements. So both results were correct and the disagreement was
entirely an under-specified artifact: the original write-up did not state that a **collection
leaf** is the required amplifier, and I generalised from the one shape I happened to pick. I then
found Shape B independently, which is a different amplifier (flat, wide, and it exits 0).

Two lessons recorded deliberately: a measurement that contradicts a colleague's is a reason to
vary the method before concluding they erred; and a performance finding is not reproducible until
its artifact pins down *which* input property does the amplifying. Both of us had half of that.

**Scope limits, stated plainly.**

- `cargo audit` was unavailable and **nothing was installed**, so the dependency dimension rests on
  manifest/lock inspection and knowledge with a May 2026 cutoff. See C5(a).
- I did **not** run the test suite. I built only `--release --bin orch` (21 s, shared target dir)
  to obtain the C0 release figures.
- C0 figures are single runs on a loaded shared machine, and mix debug and release builds (each
  row is labelled). The superlinear trend and the 3,400–31,000× gaps are orders of magnitude
  larger than build-profile or noise effects, but the absolute seconds are indicative, not
  benchmark-grade.
- Shape A's end-to-end timings and the leaf-shape A/B were produced by the delegated parsing
  audit; I verified the mechanism, the reconciliation against my own Shape B measurements, and the
  js-yaml comparison, but did not re-run its 5-minute case myself.
- TUI input handling, the orchestrator state machine and the differential harness itself were out
  of my scope (code-review and SRE gates cover them).

**Read-only compliance.** No file in `../r-orch-wt/gate-review` was modified;
no commits; no state-changing git commands; `xtask/probe-harness.sh` was not run. Probe artefacts
were written to `$TMPDIR/yamlprobe/` (outside both repos) and
deleted after the measurements. This report is the only file written.

---

## 8. Condition summary

| # | Severity | Location | Fix size | Oracle has it? |
|---|---|---|---|---|
| **C0** | **HIGH** | `orch-infra/src/storage/fs_utils.rs:215` | small | **No — regression** |
| C1 | MEDIUM | `orch-infra/src/storage/lock.rs:210` | 3 lines | No — divergence |
| C2 | MEDIUM | `orch-infra/src/storage/run.rs:184` | small | **No — regression** |
| C3 | LOW | `paths.rs:166`, `workspace/manager.rs:49` | small, additive | Yes — parity |
| C4 | LOW | `orch-domain/src/run.rs:108` | 1 line | No — new crash |
| C5 | PROCESS | no `deny.toml`/CI; `unsafe-libyaml` | config + a decision | n/a |

Three of the six are regressions or divergences the original does not have, and all three have
small, contract-safe fixes. None is an injection or a reachable traversal. **Fix C0–C4, take the
C5 decisions, and this passes.**
